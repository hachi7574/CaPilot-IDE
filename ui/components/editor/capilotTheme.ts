import { EditorView } from "@codemirror/view";
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { tags as t } from "@lezer/highlight";

/**
 * CaPilot CodeMirror theme — every value references the app's `:root` palette
 * (ui/App.css) via CSS variables, so the editor inherits the single source of
 * truth and re-skins automatically if the palette changes.
 */
const capilotThemeSpec = EditorView.theme(
  {
    "&": {
      backgroundColor: "var(--bg2)",
      color: "var(--pl-fg)",
    },
    ".cm-content": {
      caretColor: "var(--brand)",
    },
    "&.cm-focused .cm-cursor, .cm-cursor": {
      borderLeftColor: "var(--brand)",
    },
    "&.cm-focused .cm-selectionBackground, .cm-selectionBackground, .cm-content ::selection":
      {
        backgroundColor: "var(--brand-selection)",
      },
    ".cm-gutters": {
      backgroundColor: "var(--bg2)",
      color: "var(--pl-comment)",
      border: "none",
    },
    ".cm-activeLine": {
      backgroundColor: "rgb(var(--white-rgb) / 0.035)",
    },
    ".cm-activeLineGutter": {
      backgroundColor: "rgb(var(--brand-rgb) / 0.07)",
      color: "var(--brand)",
    },
    ".cm-foldPlaceholder": {
      backgroundColor: "var(--bg3)",
      border: "1px solid var(--rule2)",
      color: "var(--ink2)",
    },
    ".cm-selectionMatch": {
      backgroundColor: "rgb(var(--brand-rgb) / 0.15)",
    },
    ".cm-searchMatch": {
      backgroundColor: "var(--search-match-bg)",
    },
    ".cm-searchMatch.cm-searchMatch-selected": {
      backgroundColor: "var(--brand)",
      color: "var(--accent-ink)",
    },
    ".cm-matchingBracket": {
      backgroundColor: "rgb(var(--brand-rgb) / 0.22)",
      outline: "1px solid rgb(var(--brand-rgb) / 0.8)",
    },
    ".cm-nonmatchingBracket": {
      backgroundColor: "rgb(var(--danger-rgb) / 0.35)",
    },
    ".cm-panels": {
      backgroundColor: "var(--bg2)",
      color: "var(--ink2)",
    },
    ".cm-panels.cm-panels-top": {
      borderBottom: "1px solid var(--rule2)",
    },
    ".cm-panels.cm-panels-bottom": {
      borderTop: "1px solid var(--rule2)",
    },
    ".cm-panels input[type=text]": {
      fontFamily: "var(--mono)",
      fontSize: "12px",
      backgroundColor: "var(--bg)",
      color: "var(--ink)",
      border: "1px solid var(--rule2)",
    },
    ".cm-panels .cm-button": {
      fontFamily: "var(--pixel)",
      fontSize: "10px",
      textTransform: "uppercase",
      color: "var(--brand)",
      border: "1px solid var(--brand)",
      background: "rgb(var(--brand-rgb) / 0.06)",
    },
    ".cm-tooltip": {
      backgroundColor: "var(--bg2)",
      color: "var(--ink2)",
      border: "1px solid var(--rule2)",
      borderRadius: "0",
    },
    ".cm-tooltip .cm-tooltip-arrow:before": {
      borderTopColor: "transparent",
      borderBottomColor: "transparent",
    },
    ".cm-tooltip .cm-tooltip-arrow:after": {
      borderTopColor: "var(--bg2)",
      borderBottomColor: "var(--bg2)",
    },
    ".cm-tooltip-autocomplete > ul > li[aria-selected]": {
      backgroundColor: "rgb(var(--brand-rgb) / 0.16)",
      color: "var(--ink)",
    },
    ".cm-tooltip-autocomplete > ul > li[aria-selected] .cm-completionLabel": {
      color: "var(--ink)",
    },
    ".cm-tooltip-autocomplete > ul > li[aria-selected] .cm-completionDetail": {
      color: "var(--brand)",
    },
    ".cm-tooltip-autocomplete .cm-completionIcon": {
      color: "var(--primary)",
    },
    ".cm-tooltip-autocomplete .cm-completionDetail": {
      color: "var(--ink2)",
    },
    ".cm-tooltip .cm-completionMatchedText": {
      color: "var(--warn)",
      textDecoration: "none",
    },
    ".cm-diagnostic": {
      borderLeftWidth: "3px",
    },
    ".cm-diagnostic-error": {
      borderLeftColor: "var(--danger)",
    },
    ".cm-diagnostic-warning": {
      borderLeftColor: "var(--warn)",
    },
    ".cm-diagnosticText": {
      color: "var(--ink)",
    },
    ".cm-diagnosticAction": {
      color: "var(--brand)",
      borderColor: "var(--brand)",
    },
    ".cm-tooltip.cm-tooltip-cursor": {
      backgroundColor: "var(--bg2)",
      color: "var(--ink2)",
    },
  },
  { dark: true },
);

/** Syntax highlighting in the Atom One Dark palette (nathanbuchar/
 *  atom-one-dark-terminal). Foreground text keeps the app's --pl-fg; only the
 *  token colors are Atom One Dark. */
const capilotHighlight = HighlightStyle.define([
  { tag: t.comment, color: "var(--pl-comment)", fontStyle: "italic" },
  { tag: [t.meta, t.docComment], color: "var(--pl-comment)" },
  { tag: t.keyword, color: "var(--pl-red)" },
  { tag: t.operator, color: "var(--pl-cyan)" },
  { tag: t.punctuation, color: "var(--pl-white)" },
  { tag: [t.bool, t.null, t.atom], color: "var(--pl-red)" },
  { tag: t.number, color: "var(--pl-orange)" },
  { tag: t.string, color: "var(--pl-green)" },
  { tag: t.regexp, color: "var(--pl-green)" },
  { tag: t.tagName, color: "var(--pl-red)" },
  { tag: t.attributeName, color: "var(--pl-yellow)" },
  { tag: t.propertyName, color: "var(--pl-cyan)" },
  { tag: t.typeName, color: "var(--pl-yellow)" },
  { tag: t.className, color: "var(--pl-orange)" },
  { tag: t.namespace, color: "var(--pl-blue-purple)" },
  {
    tag: [t.function(t.variableName), t.function(t.name), t.definition(t.function(t.variableName))],
    color: "var(--pl-blue)",
  },
  {
    tag: t.definition(t.variableName),
    color: "var(--pl-white)",
  },
  { tag: t.variableName, color: "var(--pl-white)" },
  { tag: t.invalid, color: "var(--pl-red)" },
  { tag: t.heading, color: "var(--pl-yellow)" },
]);

/** Full editor extension: UI theme + syntax highlighting. */
export const capilotTheme = [capilotThemeSpec, syntaxHighlighting(capilotHighlight)];
