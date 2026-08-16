import { useCallback, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  useStore,
  defaultFileSearchState,
  type ContentSearchResult,
  type FileSearchState,
} from "../../state/store";
import { fileTab } from "../../state/openFile";

/** Debounce between keystrokes and the backend scan (Enter bypasses it). */
const SEARCH_DEBOUNCE_MS = 300;

/**
 * Content-search execution for one project root. Owns the debounce + request-id
 * race control (a stale async response never overwrites a newer query), calls
 * the `fs_search` backend, and keeps the store slice for the root up to date.
 *
 * State lives in the store keyed by root so switching right-sidebar tabs (which
 * unmounts FilesPanel) preserves the query and results.
 */
export function useFileContentSearch(root: string) {
  const state =
    useStore((s) => s.fileSearchByRoot[root]) ?? defaultFileSearchState();
  const update = useStore((s) => s.updateFileSearch);
  const clearStore = useStore((s) => s.clearFileSearch);
  const addTab = useStore((s) => s.addTab);
  const requestReveal = useStore((s) => s.requestReveal);

  const debounceRef = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  // Monotonic request id: only the response whose id matches the latest gets
  // written back (older scans resolve after a newer keystroke and are dropped).
  const idRef = useRef(0);

  const run = useCallback(
    (q: string, opts: { caseSensitive: boolean; wholeWord: boolean; useRegex: boolean; include: string; exclude: string }, immediate: boolean) => {
      if (debounceRef.current) {
        clearTimeout(debounceRef.current);
        debounceRef.current = undefined;
      }
      const start = () => {
        const id = ++idRef.current;
        if (q.trim() === "") {
          update(root, { query: q, results: null, loading: false, searchId: id });
          return;
        }
        update(root, { query: q, loading: true, searchId: id });
        invoke<ContentSearchResult>("fs_search", {
          rootPath: root,
          query: q,
          caseSensitive: opts.caseSensitive,
          wholeWord: opts.wholeWord,
          useRegex: opts.useRegex,
          includePattern: opts.include.trim() || null,
          excludePattern: opts.exclude.trim() || null,
        })
          .then((results) => {
            if (idRef.current !== id) return; // superseded by a newer query
            update(root, { results, loading: false });
          })
          .catch((err) => {
            if (idRef.current !== id) return;
            console.error("fs_search failed:", err);
            update(root, { results: null, loading: false });
          });
      };
      if (immediate) start();
      else debounceRef.current = setTimeout(start, SEARCH_DEBOUNCE_MS);
    },
    [root, update]
  );

  const optsOf = (s: FileSearchState) => ({
    caseSensitive: s.caseSensitive,
    wholeWord: s.wholeWord,
    useRegex: s.useRegex,
    include: s.includePattern,
    exclude: s.excludePattern,
  });

  /** Debounced query change (input typing). */
  const setQuery = useCallback(
    (q: string) => {
      run(q, optsOf(state), false);
    },
    [run, state]
  );

  /** Update the query text only (no backend run) — used mid-IME-composition so
   *  keystrokes while selecting a candidate don't fire searches. */
  const setQueryQuiet = useCallback(
    (q: string) => {
      update(root, { query: q });
    },
    [update, root]
  );

  /** Immediate re-run (Enter key / mode toggles). */
  const submit = useCallback(() => {
    run(state.query, optsOf(state), true);
  }, [run, state]);

  const setCaseSensitive = useCallback(
    (v: boolean) => {
      update(root, { caseSensitive: v });
      run(state.query, { ...optsOf(state), caseSensitive: v }, true);
    },
    [run, update, root, state]
  );
  const setWholeWord = useCallback(
    (v: boolean) => {
      update(root, { wholeWord: v });
      run(state.query, { ...optsOf(state), wholeWord: v }, true);
    },
    [run, update, root, state]
  );
  const setUseRegex = useCallback(
    (v: boolean) => {
      update(root, { useRegex: v });
      run(state.query, { ...optsOf(state), useRegex: v }, true);
    },
    [run, update, root, state]
  );
  const setIncludePattern = useCallback(
    (v: string) => {
      update(root, { includePattern: v });
      run(state.query, { ...optsOf(state), include: v }, false);
    },
    [run, update, root, state]
  );
  const setExcludePattern = useCallback(
    (v: string) => {
      update(root, { excludePattern: v });
      run(state.query, { ...optsOf(state), exclude: v }, false);
    },
    [run, update, root, state]
  );

  const toggleCollapsed = useCallback(
    (filePath: string) => {
      update(root, {
        collapsed: { ...state.collapsed, [filePath]: !state.collapsed[filePath] },
      });
    },
    [update, root, state.collapsed]
  );

  const clear = useCallback(() => {
    if (debounceRef.current) {
      clearTimeout(debounceRef.current);
      debounceRef.current = undefined;
    }
    idRef.current++;
    clearStore(root);
  }, [root, clearStore]);

  /** Open a match in the editor and scroll to its line. */
  const openMatch = useCallback(
    (filePath: string, line: number, column?: number) => {
      const name = filePath.split("/").pop() || filePath;
      addTab(fileTab(filePath, name));
      requestReveal(filePath, line, column);
    },
    [addTab, requestReveal]
  );

  // Cancel any pending debounce / drop in-flight responses on unmount.
  useEffect(
    () => () => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
      idRef.current++;
    },
    []
  );

  return {
    state,
    setQuery,
    setQueryQuiet,
    submit,
    setCaseSensitive,
    setWholeWord,
    setUseRegex,
    setIncludePattern,
    setExcludePattern,
    toggleCollapsed,
    clear,
    openMatch,
  };
}
