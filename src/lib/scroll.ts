export const SCROLL_TO_TOP_REVEAL_PX = 480;

interface ScrollMetrics {
  scrollTop: number;
  scrollHeight: number;
  clientHeight: number;
}

export function shouldShowScrollToTop(metrics: ScrollMetrics): boolean {
  return metrics.scrollHeight > metrics.clientHeight
    && metrics.scrollTop >= SCROLL_TO_TOP_REVEAL_PX;
}

interface ScrollAnchorMetrics {
  currentScrollTop: number;
  containerTop: number;
  rowTop: number;
  savedRowOffset: number;
}

export function anchoredScrollTop({
  currentScrollTop,
  containerTop,
  rowTop,
  savedRowOffset,
}: ScrollAnchorMetrics): number {
  return Math.max(0, currentScrollTop + rowTop - containerTop - savedRowOffset);
}

interface RouteScrollTargetOptions {
  fromName: string | symbol | null | undefined;
  toName: string | symbol | null | undefined;
  fromPage: string;
  toPage: string;
  capturesListScrollTop: number;
}

export function routeScrollTarget({
  fromName,
  toName,
  fromPage,
  toPage,
  capturesListScrollTop,
}: RouteScrollTargetOptions): number | null {
  if (fromName === "capture-detail" && toName === "captures") {
    return capturesListScrollTop;
  }
  return fromPage === toPage ? null : 0;
}
