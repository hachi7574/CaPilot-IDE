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
      backgroundColor: "var(--bg)",
      color: "var(--ink)",
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
      backgroundColor: "var(--bg)",
      color: "var(--ink2)",
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

/** Syntax highlighting driven by the CaPilot palette. */
const capilotHighlight = HighlightStyle.define([
  { tag: t.comment, color: "var(--ink2)", fontStyle: "italic" },
  { tag: [t.meta, t.docComment], color: "var(--ink2)" },
  { tag: t.keyword, color: "var(--primary)" },
  { tag: t.operator, color: "var(--brand)" },
  { tag: t.punctuation, color: "var(--ink2)" },
  { tag: [t.bool, t.null, t.atom], color: "var(--lane-1)" },
  { tag: t.number, color: "var(--lane-2)" },
  { tag: t.string, color: "var(--success)" },
  { tag: t.regexp, color: "var(--success)" },
  { tag: t.tagName, color: "var(--brand)" },
  { tag: t.attributeName, color: "var(--warn)" },
  { tag: t.propertyName, color: "var(--ai)" },
  { tag: t.typeName, color: "var(--lane-1)" },
  { tag: t.className, color: "var(--lane-2)" },
  { tag: t.namespace, color: "var(--lane-5)" },
  {
    tag: [t.function(t.variableName), t.function(t.name), t.definition(t.function(t.variableName))],
    color: "var(--ai)",
  },
  {
    tag: t.definition(t.variableName),
    color: "var(--ink)",
  },
  { tag: t.variableName, color: "var(--ink2)" },
  { tag: t.invalid, color: "var(--danger)" },
  { tag: t.heading, color: "var(--brand)" },
]);

/** Full editor extension: UI theme + syntax highlighting. */
export const capilotTheme = [capilotThemeSpec, syntaxHighlighting(capilotHighlight)];
