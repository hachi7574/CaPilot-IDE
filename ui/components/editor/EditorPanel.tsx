import { useCallback, useEffect, useRef, useState } from "react";
import {
  EditorView,
  keymap,
  lineNumbers,
  highlightActiveLine,
  Decoration,
  ViewPlugin,
  type DecorationSet,
  type ViewUpdate,
} from "@codemirror/view";
import { EditorState, RangeSetBuilder } from "@codemirror/state";
import { defaultKeymap } from "@codemirror/commands";
import {
  search,
  findNext,
  findPrevious,
  replaceNext,
  replaceAll,
  getSearchQuery,
  SearchQuery,
  setSearchQuery,
  highlightSelectionMatches,
} from "@codemirror/search";
import { javascript } from "@codemirror/lang-javascript";
import { rust } from "@codemirror/lang-rust";
import { python } from "@codemirror/lang-python";
import { invoke } from "@tauri-apps/api/core";
import { useStore } from "../../state/store";
import { Icon } from "../Icon";
import { capilotTheme } from "./capilotTheme";

interface EditorPanelProps {
  filePath: string;
  /** True when this panel belongs to the active tab. Only an active editor may
   *  respond to the window-routed Ctrl+F search directive. */
  active?: boolean;
}

const LANG_MAP: Record<string, () => any> = {
  rs: rust,
  py: python,
};

function getLangExtension(filePath: string) {
  const ext = filePath.split(".").pop() ?? "";
  const fn = LANG_MAP[ext];
  if (fn) return fn();
  return javascript({ typescript: ext === "ts" || ext === "tsx" });
}

/** Last `searchRequest.seq` an active editor has consumed for Ctrl+F search. */
let lastSearchHandledSeq = 0;

/** Editor Ctrl+F match highlight marks. Colors mirror the terminal SearchAddon
 *  (`--search-match-bg` for matches, `--brand` for the active one). */
const searchMatchMark = Decoration.mark({ class: "cm-editor-search-match" });
const activeSearchMatchMark = Decoration.mark({ class: "cm-editor-search-match-active" });

/** Paint highlights for the editor's current search query without relying on
 *  CodeMirror's native search panel. The `search()` extension's own highlighter
 *  bails when its panel is closed (`highlight` returns `Decoration.none` when
 *  `panel` is null), so this plugin reads the query from `getSearchQuery` and
 *  paints its own marks across the visible range — the same effect the
 *  terminal's SearchAddon decorations give the PTY. */
const editorSearchHighlighter = ViewPlugin.fromClass(
  class {
    decorations: DecorationSet;

    constructor(view: EditorView) {
      this.decorations = this.compute(view);
    }

    update(update: ViewUpdate) {
      if (
        update.docChanged ||
        update.selectionSet ||
        update.viewportChanged ||
        update.geometryChanged ||
        !getSearchQuery(update.startState).eq(getSearchQuery(update.state))
      ) {
        this.decorations = this.compute(update.view);
      }
    }

    compute(view: EditorView): DecorationSet {
      const query = getSearchQuery(view.state);
      if (!query.valid) return Decoration.none;
      const sel = view.state.selection.main;
      const builder = new RangeSetBuilder<Decoration>();
      for (const { from, to } of view.visibleRanges) {
        const cursor = query.getCursor(view.state, from, to);
        let step = cursor.next();
        while (!step.done) {
          const { from: mFrom, to: mTo } = step.value;
          const active = mFrom === sel.from && mTo === sel.to;
          builder.add(mFrom, mTo, active ? activeSearchMatchMark : searchMatchMark);
          step = cursor.next();
        }
      }
      return builder.finish();
    }
  },
  { decorations: (v) => v.decorations }
);

export function EditorPanel({ filePath, active = true }: EditorPanelProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const searchRequest = useStore((s) => s.searchRequest);
  const revealRequest = useStore((s) => s.revealRequest);
  // One-shot reveal (from a content-search result click): the request may arrive
  // while the document is still loading, so it's stashed in a ref and applied
  // either immediately or right after the view is created in `loadFile`.
  const pendingRevealRef = useRef<{ line: number; column?: number } | null>(null);
  const lastRevealSeqRef = useRef(0);

  // Editor Ctrl+F search bar state (mirrors XTermPanel). The query lives on the
  // React side; it is pushed into CodeMirror via `setSearchQuery` only when the
  // user searches, so the native panel never opens.
  const [searchOpen, setSearchOpen] = useState(false);
  const [searchQuery, setSearchQueryText] = useState("");
  const [searchResults, setSearchResults] = useState<{
    resultIndex: number;
    resultCount: number;
  } | null>(null);
  const searchInputRef = useRef<HTMLInputElement>(null);
  // Refs mirroring the search state so the one-time CodeMirror keymap closure
  // (built per file load) always sees the current open/query values.
  const searchOpenRef = useRef(false);
  const searchQueryRef = useRef("");

  // Replace mode (Ctrl+Shift+F / the 替换 button): a second row in the same
  // floating bar. The replacement text lives on the React side and is folded
  // into the SearchQuery when replaceNext/replaceAll run.
  const [replaceOpen, setReplaceOpen] = useState(false);
  const [replaceText, setReplaceText] = useState("");
  const replaceTextRef = useRef("");
  const replaceInputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    searchOpenRef.current = searchOpen;
  }, [searchOpen]);

  useEffect(() => {
    searchQueryRef.current = searchQuery;
  }, [searchQuery]);

  useEffect(() => {
    replaceTextRef.current = replaceText;
  }, [replaceText]);

  /** Recompute the match counter (`n/N`) from the editor's current search query
   *  and selection. Used after every find/replace step. */
  const refreshSearchResults = useCallback((view: EditorView) => {
    const query = getSearchQuery(view.state);
    if (!query.valid) {
      setSearchResults(null);
      return;
    }
    const matches: { from: number; to: number }[] = [];
    const cursor = query.getCursor(view.state);
    let step = cursor.next();
    while (!step.done) {
      matches.push(step.value);
      step = cursor.next();
    }
    const sel = view.state.selection.main;
    const index = matches.findIndex((m) => m.from === sel.from && m.to === sel.to);
    setSearchResults({ resultIndex: index, resultCount: matches.length });
  }, []);

  /** Run the editor search: push the query into CodeMirror (which paints match
   *  highlights via `editorSearchHighlighter`), jump to the next/previous match,
   *  and report the `n/N` counter. An empty query clears highlights. */
  const runFind = useCallback(
    (query: string, dir: "next" | "prev") => {
      const view = viewRef.current;
      if (!view) return;
      if (!query) {
        view.dispatch({ effects: setSearchQuery.of(new SearchQuery({ search: "" })) });
        setSearchResults(null);
        return;
      }
      const q = new SearchQuery({ search: query, replace: replaceTextRef.current });
      view.dispatch({ effects: setSearchQuery.of(q) });
      if (dir === "next") findNext(view);
      else findPrevious(view);
      refreshSearchResults(view);
    },
    [refreshSearchResults]
  );

  /** Replace the current match (or jump to the next one if the selection is not
   *  on a match) — CodeMirror's `replaceNext` semantics, without its panel. */
  const doReplaceNext = useCallback(() => {
    const view = viewRef.current;
    if (!view || !searchQueryRef.current) return;
    const q = new SearchQuery({
      search: searchQueryRef.current,
      replace: replaceTextRef.current,
    });
    view.dispatch({ effects: setSearchQuery.of(q) });
    if (replaceNext(view)) refreshSearchResults(view);
    else setSearchResults({ resultIndex: -1, resultCount: 0 });
  }, [refreshSearchResults]);

  /** Replace every occurrence, then land on the first remaining match so the
   *  counter stays meaningful (0/无结果 when nothing is left). */
  const doReplaceAll = useCallback(() => {
    const view = viewRef.current;
    if (!view || !searchQueryRef.current) return;
    const q = new SearchQuery({
      search: searchQueryRef.current,
      replace: replaceTextRef.current,
    });
    view.dispatch({ effects: setSearchQuery.of(q) });
    if (replaceAll(view)) {
      if (findNext(view)) refreshSearchResults(view);
      else setSearchResults({ resultIndex: -1, resultCount: 0 });
    }
  }, [refreshSearchResults]);

  /** Close the search bar: clear highlights (empty query), drop the counter,
   *  and return focus to the editor. The query text is kept so reopening it
   *  restores the term — same as the terminal. */
  const closeSearch = useCallback(() => {
    setSearchOpen(false);
    setReplaceOpen(false);
    const view = viewRef.current;
    if (view) {
      view.dispatch({ effects: setSearchQuery.of(new SearchQuery({ search: "" })) });
    }
    setSearchResults(null);
    viewRef.current?.focus();
  }, []);

  // Opening the bar focuses the query input and selects the old text so typing
  // a new term replaces it (re-search is a single keystroke).
  useEffect(() => {
    if (!searchOpen) return;
    const raf = requestAnimationFrame(() => {
      searchInputRef.current?.focus();
      searchInputRef.current?.select();
    });
    return () => cancelAnimationFrame(raf);
  }, [searchOpen]);

  // Toggling replace mode focuses the replacement input.
  useEffect(() => {
    if (!replaceOpen) return;
    const raf = requestAnimationFrame(() => {
      replaceInputRef.current?.focus();
      replaceInputRef.current?.select();
    });
    return () => cancelAnimationFrame(raf);
  }, [replaceOpen]);

  // While the bar is open, F3/Shift+F3 navigate matches from anywhere in the
  // editor (the input's own Enter/Shift+Enter cover the input-focused case, and
  // CodeMirror's keymap covers the editor-focused case — guard against double
  // handling by skipping when focus is inside `.cm-editor`).
  useEffect(() => {
    if (!searchOpen) return;
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key !== "F3") return;
      const el = document.activeElement as HTMLElement | null;
      if (el?.closest?.(".cm-editor")) return;
      e.preventDefault();
      runFind(searchQuery, e.shiftKey ? "prev" : "next");
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [searchOpen, searchQuery, runFind]);

  /** Scroll the cursor to a stashed content-search reveal (line + optional
   *  column), if one is pending. Clamps to the document. */
  const applyPendingReveal = useCallback((view: EditorView) => {
    const pr = pendingRevealRef.current;
    if (!pr) return;
    pendingRevealRef.current = null;
    const line = Math.max(1, Math.min(pr.line, view.state.doc.lines));
    const lineObj = view.state.doc.line(line);
    const col = pr.column ? Math.min(pr.column, lineObj.length) : 0;
    const pos = lineObj.from + col;
    view.dispatch({
      selection: { anchor: pos },
      effects: EditorView.scrollIntoView(pos, { y: "center" }),
    });
  }, []);

  // Content-search reveal routed from a result click: stash the target and apply
  // once this file's document is (or becomes) available.
  useEffect(() => {
    if (!active || !revealRequest) return;
    if (revealRequest.filePath !== filePath) return;
    if (revealRequest.seq <= lastRevealSeqRef.current) return;
    lastRevealSeqRef.current = revealRequest.seq;
    pendingRevealRef.current = { line: revealRequest.line, column: revealRequest.column };
    const view = viewRef.current;
    if (view) applyPendingReveal(view);
  }, [revealRequest, active, filePath, applyPendingReveal]);

  useEffect(() => {
    if (!containerRef.current) return;

    // Guard against the async load race: opening file A then file B before A's
    // `fs_read` resolves would otherwise mount two .cm-editor into one container
    // (view_A leaks and autosaves into file A while the user edits B). The
    // cancelled flag is set by cleanup, so a superseded load bails on resolve.
    let cancelled = false;

    const loadFile = async () => {
      let content = "";
      try {
        content = await invoke<string>("fs_read", { path: filePath });
      } catch {
        content = `// Could not read: ${filePath}`;
      }
      if (cancelled) return; // superseded by a newer file / unmount

      const updateListener = EditorView.updateListener.of((update) => {
        if (update.docChanged) {
          // Autosave with debounce. Capture the path this view was created for
          // so a pending timer always writes to the correct file.
          const savedPath = filePath;
          clearTimeout(debounceRef.current);
          debounceRef.current = setTimeout(() => {
            const text = update.state.doc.toString();
            invoke("fs_write", { path: savedPath, content: text }).catch(console.error);
          }, 800);
        }
      });

      const state = EditorState.create({
        doc: content,
        extensions: [
          lineNumbers(),
          highlightActiveLine(),
          // Custom search bindings first: they replace the `searchKeymap`
          // entries — Ctrl+F opens the floating bar (never CodeMirror's native
          // panel), Ctrl+Shift+F toggles its replace row, F3/Mod-g navigate,
          // Escape closes the bar. They must precede `defaultKeymap` so Escape
          // closes the bar instead of running `simplifySelection` when a match
          // is selected. `search()` stays installed because findNext/findPrevious
          // and the highlighter read its state.
          keymap.of([
            {
              key: "Mod-f",
              run: () => {
                setSearchOpen(true);
                return true;
              },
            },
            {
              key: "Mod-Shift-f",
              run: () => {
                setSearchOpen(true);
                setReplaceOpen((v) => !v);
                return true;
              },
            },
            {
              key: "Escape",
              run: () => {
                if (!searchOpenRef.current) return false;
                closeSearch();
                return true;
              },
            },
            {
              key: "F3",
              preventDefault: true,
              run: () => {
                if (!searchOpenRef.current || !searchQueryRef.current) return false;
                runFind(searchQueryRef.current, "next");
                return true;
              },
              shift: () => {
                if (!searchOpenRef.current || !searchQueryRef.current) return false;
                runFind(searchQueryRef.current, "prev");
                return true;
              },
            },
            {
              key: "Mod-g",
              preventDefault: true,
              run: () => {
                if (!searchQueryRef.current) return false;
                runFind(searchQueryRef.current, "next");
                return true;
              },
            },
            {
              key: "Mod-Shift-g",
              preventDefault: true,
              run: () => {
                if (!searchQueryRef.current) return false;
                runFind(searchQueryRef.current, "prev");
                return true;
              },
            },
          ]),
          keymap.of(defaultKeymap),
          search(),
          highlightSelectionMatches(),
          editorSearchHighlighter,
          capilotTheme,
          getLangExtension(filePath),
          updateListener,
        ],
      });

      // Destroy any existing view before creating a new one so there is never
      // more than one .cm-editor in the container.
      viewRef.current?.destroy();
      const view = new EditorView({
        state,
        parent: containerRef.current!,
      });

      viewRef.current = view;
      // A content-search reveal may have arrived while this file was loading.
      applyPendingReveal(view);
    };

    loadFile();

    return () => {
      cancelled = true;
      // Flush a pending autosave instead of only clearing the timer, so edits
      // within 800ms of closing the editor aren't lost.
      const pending = debounceRef.current;
      debounceRef.current = undefined;
      if (pending) {
        clearTimeout(pending);
        const view = viewRef.current;
        if (view) {
          const text = view.state.doc.toString();
          invoke("fs_write", { path: filePath, content: text }).catch(console.error);
        }
      }
      viewRef.current?.destroy();
      viewRef.current = null;
    };
  }, [filePath, runFind, closeSearch, applyPendingReveal]);

  // Ctrl+F routed from the window (when the editor itself does not have focus).
  // Opens the floating search bar on the active tab's editor.
  useEffect(() => {
    if (!active || !searchRequest) return;
    if (searchRequest.target !== "editor") return;
    if (searchRequest.seq <= lastSearchHandledSeq) return;
    lastSearchHandledSeq = searchRequest.seq;
    setSearchOpen(true);
  }, [searchRequest, active]);

  return (
    // The wrapper is the positioned ancestor for the floating search bar (the
    // inner container scrolls the document, so the bar stays pinned top-right
    // exactly like the terminal's overlay).
    <div
      style={{
        position: "relative",
        flex: 1,
        minHeight: 0,
        overflow: "hidden",
      }}
    >
      <div
        ref={containerRef}
        style={{
          width: "100%",
          height: "100%",
          overflow: "auto",
        }}
      />
      {searchOpen && (
        <div className="term-search-bar">
          <button
            className={`term-search-repl-toggle${replaceOpen ? " is-open" : ""}`}
            title="替换模式 (Ctrl+Shift+F)"
            onClick={() => setReplaceOpen((v) => !v)}
          >
            {replaceOpen ? (
              <Icon name="chevron-up" size={12} />
            ) : (
              <Icon name="chevron-right" size={12} />
            )}
          </button>
          <div className="term-search-body">
            <div className="term-search-row">
              <input
                ref={searchInputRef}
                className="term-search-input"
                type="text"
                placeholder="搜索文本…"
                value={searchQuery}
                onChange={(e) => {
                  const q = e.target.value;
                  setSearchQueryText(q);
                  searchQueryRef.current = q;
                  // runFind handles the empty query by clearing highlights.
                  runFind(q, "next");
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    e.stopPropagation();
                    runFind(searchQuery, e.shiftKey ? "prev" : "next");
                  } else if (e.key === "Escape") {
                    e.preventDefault();
                    e.stopPropagation();
                    closeSearch();
                  }
                }}
              />
              <span className="term-search-count">
                {searchQuery && searchResults && searchResults.resultCount > 0
                  ? `${Math.max(searchResults.resultIndex, 0) + 1}/${searchResults.resultCount}`
                  : searchQuery
                    ? "无结果"
                    : ""}
              </span>
              <button
                className="term-search-btn"
                title="上一个匹配 (Shift+Enter / Shift+F3)"
                onClick={() => runFind(searchQuery, "prev")}
                disabled={!searchQuery}
              >
                <Icon name="arrow-up" size={12} />
              </button>
              <button
                className="term-search-btn"
                title="下一个匹配 (Enter / F3)"
                onClick={() => runFind(searchQuery, "next")}
                disabled={!searchQuery}
              >
                <Icon name="arrow-down" size={12} />
              </button>
              <button
                className="term-search-close"
                title="关闭搜索 (Esc)"
                onClick={closeSearch}
              >
                ×
              </button>
            </div>
            {replaceOpen && (
              <div className="term-search-row">
              <input
                ref={replaceInputRef}
                className="term-search-input"
                type="text"
                placeholder="替换为…"
                value={replaceText}
                onChange={(e) => {
                  const v = e.target.value;
                  setReplaceText(v);
                  replaceTextRef.current = v;
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    e.stopPropagation();
                    doReplaceNext();
                  } else if (e.key === "Escape") {
                    e.preventDefault();
                    e.stopPropagation();
                    closeSearch();
                  }
                }}
              />
              <button
                className="term-search-btn"
                title="替换当前匹配并跳到下一个"
                onClick={doReplaceNext}
                disabled={!searchQuery}
              >
                替换
              </button>
              <button
                className="term-search-btn"
                title="替换所有匹配"
                onClick={doReplaceAll}
                disabled={!searchQuery}
              >
                全部替换
              </button>
            </div>
          )}
          </div>
        </div>
      )}
    </div>
  );
}
