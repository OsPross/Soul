/**
 * Collapsed-mode content. Priority: confirm > text result > timer toast > live activity >
 * toast > update notice > caption > music > idle. A pending confirmation always wins and holds
 * the card: hover never turns it into peek and nothing expands it until it is answered.
 * `stage` = big live-activity card, `compact` = same info squeezed into the pill (quiet mode
 * or "don't expand" for that feature).
 *
 * Pure (no imports) so `tests/ui/stage.test.ts` can run it under plain Node.
 */
export type Stage = "confirm" | "result" | "toast" | "stage" | "compact" | "update" | "caption" | "music" | "idle";

export interface StageInput {
  /** Assistant banner state currently shown (`idle`, `listening`, `confirm`, `done`, …). */
  state: string;
  kind: string | null;
  textResult: boolean;
  /** Kind of the toast on screen, `null` without one. */
  toastKind: string | null;
  expandAllowed: boolean;
  showCaption: boolean;
  updateNotice: boolean;
  mediaActive: boolean;
}

export function pickStage(i: StageInput): Stage {
  if (i.state === "confirm") return "confirm";
  if (i.textResult) return "result";
  if (i.toastKind === "timer") return "toast";
  const active = i.state !== "idle";
  if (active && !(i.state === "done" && i.kind === "dictation" && i.showCaption)) {
    return i.expandAllowed ? "stage" : "compact";
  }
  if (i.toastKind) return "toast";
  if (i.updateNotice) return "update";
  if (i.showCaption) return "caption";
  if (i.mediaActive) return "music";
  return "idle";
}

/** Cards with buttons stay put: hover does not peek, wheel/click do not expand. */
export function holdsCard(stage: Stage, toastHasAction: boolean): boolean {
  return stage === "confirm" || stage === "result" || (stage === "toast" && toastHasAction);
}

/** A confirmation arriving while the Island is open (peek / expanded) collapses it onto the card. */
export function mustCollapseFor(stage: Stage, mode: "collapsed" | "peek" | "expanded"): boolean {
  return stage === "confirm" && mode !== "collapsed";
}
