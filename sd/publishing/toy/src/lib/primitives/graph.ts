/**
 * Graph core primitive
 * Base rendering engine for the rafters chart system.
 *
 * Provides SVG/Canvas container creation, scale helpers, coordinate transforms,
 * and path builders that chart-type primitives (gauge, bar, line, etc.) build on.
 *
 * Leaf primitive: zero external deps, framework-agnostic, SSR-safe.
 */

export interface GraphConfig {
  container: HTMLElement;
  width?: number;
  height?: number;
  renderer?: 'svg' | 'canvas';
}

export interface GraphControls {
  resize: (width: number, height: number) => void;
  destroy: () => void;
  readonly element: SVGSVGElement | HTMLCanvasElement | null;
}

export interface BandScaleOptions {
  paddingInner?: number;
  paddingOuter?: number;
  /** Distributes leftover range; 0.5 (default) centers bands, like d3-scale. */
  align?: number;
}

export interface BandScale<T extends string> {
  /** Returns the left edge of the band for a given category value. */
  scale: (value: T) => number;
  /** Returns the width of each band (excluding padding). */
  bandwidth: () => number;
  /** Returns the distance between the starts of adjacent bands. */
  step: () => number;
  domain: readonly T[];
  range: readonly [number, number];
}

export interface GridLines {
  horizontal: Array<{ y: number; x1: number; x2: number }>;
  vertical: Array<{ x: number; y1: number; y2: number }>;
}

const DEFAULT_WIDTH = 300;
const DEFAULT_HEIGHT = 200;

/**
 * Create a graph rendering context.
 * Appends an SVG or Canvas element to the container.
 */
export function createGraph(config: GraphConfig): GraphControls {
  if (!config.container) {
    throw new Error('Graph container is required');
  }

  const { container, renderer = 'svg' } = config;
  let width = config.width ?? DEFAULT_WIDTH;
  let height = config.height ?? DEFAULT_HEIGHT;
  let element: SVGSVGElement | HTMLCanvasElement | null = null;

  if (renderer === 'svg') {
    const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
    svg.setAttribute('viewBox', `0 0 ${width} ${height}`);
    svg.setAttribute('width', '100%');
    svg.setAttribute('height', '100%');
    svg.style.display = 'block';
    svg.setAttribute('role', 'img');
    container.appendChild(svg);
    element = svg;
  } else {
    const canvas = document.createElement('canvas');
    canvas.width = width;
    canvas.height = height;
    canvas.style.display = 'block';
    canvas.style.width = '100%';
    canvas.style.height = '100%';
    container.appendChild(canvas);
    element = canvas;
  }

  return {
    get element() {
      return element;
    },

    resize(newWidth: number, newHeight: number) {
      width = newWidth;
      height = newHeight;

      if (!element) return;

      if (element instanceof SVGSVGElement) {
        element.setAttribute('viewBox', `0 0 ${width} ${height}`);
      } else {
        element.width = width;
        element.height = height;
      }
    },

    destroy() {
      if (element) {
        element.remove();
        element = null;
      }
    },
  };
}

/**
 * Create a linear scale that maps values from domain to range.
 * Handles inverted domains (domain[0] > domain[1]).
 */
export function linearScale(
  domain: [number, number],
  range: [number, number],
): (value: number) => number {
  const domainSpan = domain[1] - domain[0];
  const rangeSpan = range[1] - range[0];

  if (domainSpan === 0) {
    return () => range[0];
  }

  return (value: number) => {
    const normalized = (value - domain[0]) / domainSpan;
    return range[0] + normalized * rangeSpan;
  };
}

/**
 * Create a band scale that maps categorical values to evenly-spaced bands,
 * reproducing d3-scale scaleBand: step = span / (n - paddingInner + 2*paddingOuter),
 * bandwidth = step * (1 - paddingInner), and leftover range distributed by align
 * (default 0.5 centers). paddingInner/paddingOuter/align are fractions in [0,1].
 */
export function bandScale<T extends string>(
  domain: readonly T[],
  range: readonly [number, number],
  opts?: BandScaleOptions,
): BandScale<T> {
  const rangeSpan = range[1] - range[0];
  const count = domain.length;
  const pInner = opts?.paddingInner ?? 0;
  const pOuter = opts?.paddingOuter ?? 0;
  const align = opts?.align ?? 0.5;

  // d3 denominator: n - paddingInner + 2*paddingOuter (clamped so a single band divides by >= 1).
  const stepVal = count === 0 ? 0 : rangeSpan / Math.max(1, count - pInner + 2 * pOuter);
  const bw = stepVal * (1 - pInner);

  // Distribute leftover range by align (0.5 centers); firstEdge is the left edge of band 0.
  const firstEdge = range[0] + (rangeSpan - stepVal * (count - pInner)) * align;

  const indexMap = new Map<T, number>();
  for (let i = 0; i < domain.length; i++) {
    indexMap.set(domain[i] as T, i);
  }

  return {
    scale(value: T): number {
      const idx = indexMap.get(value);
      if (idx === undefined) return range[0];
      return firstEdge + idx * stepVal;
    },
    bandwidth: () => bw,
    step: () => stepVal,
    domain,
    range,
  };
}

/**
 * d3-array tickSpec: the [firstIndex, lastIndex, signedIncrement] of the nice
 * ticks. A negative increment signals the sub-integer branch (multiply by 1/inc
 * rather than divide, to avoid float drift). Retries at double the count when the
 * indices collapse (i2 < i1) for a near-1 count, so an ordinary domain never comes
 * back empty at count=1.
 */
function tickSpec(start: number, stop: number, count: number): [number, number, number] {
  const e10 = Math.sqrt(50);
  const e5 = Math.sqrt(10);
  const e2 = Math.sqrt(2);
  const step = (stop - start) / count;
  const power = Math.floor(Math.log10(step));
  const error = step / Math.pow(10, power);
  const factor = error >= e10 ? 10 : error >= e5 ? 5 : error >= e2 ? 2 : 1;
  let i1: number;
  let i2: number;
  let inc: number;
  if (power < 0) {
    inc = Math.pow(10, -power) / factor;
    i1 = Math.round(start * inc);
    i2 = Math.round(stop * inc);
    if (i1 / inc < start) ++i1;
    if (i2 / inc > stop) --i2;
    inc = -inc;
  } else {
    inc = Math.pow(10, power) * factor;
    i1 = Math.round(start / inc);
    i2 = Math.round(stop / inc);
    if (i1 * inc < start) ++i1;
    if (i2 * inc > stop) --i2;
  }
  if (i2 < i1 && count >= 0.5 && count < 2) return tickSpec(start, stop, count * 2);
  return [i1, i2, inc];
}

/**
 * Generate nicely-rounded tick values for a numeric axis.
 * Uses d3-array's 1/2/5 x 10^n "nice number" rule with d3's exact factor
 * thresholds (sqrt(2), sqrt(10), sqrt(50)), so the step matches what d3 picks
 * across the input space, not only for round cases: ticks(0,100,5) -> step 20
 * ([0,20,40,60,80,100]); 25 (2.5 x 10) is never chosen.
 */
export function ticks(min: number, max: number, count: number): number[] {
  if (count <= 0 || min === max) return [min];

  // Index-based generation (d3-array ticks): each tick is a rounded integer index
  // times the step, so the upper bound is never dropped by accumulated float error.
  const [i1, i2, inc] = tickSpec(min, max, count);
  if (i2 < i1) return [];
  const result: number[] = [];
  if (inc < 0) {
    const stepRecip = -inc;
    for (let i = i1; i <= i2; i++) result.push(i / stepRecip);
  } else {
    for (let i = i1; i <= i2; i++) result.push(i * inc);
  }
  return result;
}

/**
 * Generate gridline coordinates from tick arrays.
 */
export function gridLines(
  xTicks: number[],
  yTicks: number[],
  plotArea: { x1: number; y1: number; x2: number; y2: number },
): GridLines {
  return {
    horizontal: yTicks.map((y) => ({
      y,
      x1: plotArea.x1,
      x2: plotArea.x2,
    })),
    vertical: xTicks.map((x) => ({
      x,
      y1: plotArea.y1,
      y2: plotArea.y2,
    })),
  };
}

/**
 * Convert polar coordinates to cartesian.
 * Angle in degrees, 0 = right (3 o'clock), counterclockwise.
 */
export function polarToCartesian(
  cx: number,
  cy: number,
  radius: number,
  angleDeg: number,
): { x: number; y: number } {
  const angleRad = (angleDeg * Math.PI) / 180;
  return {
    x: cx + radius * Math.cos(angleRad),
    y: cy - radius * Math.sin(angleRad),
  };
}

/**
 * Build a straight-line SVG path from a series of points.
 */
export function linePath(points: { x: number; y: number }[]): string {
  if (points.length === 0) return '';

  const segments = points.map((p, i) => {
    const cmd = i === 0 ? 'M' : 'L';
    return `${cmd} ${p.x} ${p.y}`;
  });

  return segments.join(' ');
}

/** `sign(x)`, ported verbatim from d3-shape's `curve/monotone.js` -- `0` is
 *  never a distinct case (matches `x < 0 ? -1 : 1`, including for `-0`/`NaN`,
 *  which both take the `1` branch there too). */
function monotoneSign(x: number): number {
  return x < 0 ? -1 : 1;
}

/**
 * Steffen's one-dimensional monotonic slope estimate at the MIDDLE of three
 * points (Steffen 1990, as cited in d3-shape's `curve/monotone.js` `slope3`).
 * Ported member-for-member, including the `-0` denominator substitution that
 * gives a degenerate (zero-width) step the correctly-signed infinite slope --
 * simplifying it away would change which points end up +/-Infinity before the
 * `Math.min` clamps them.
 */
function monotoneSlope3(
  x0: number,
  y0: number,
  x1: number,
  y1: number,
  x2: number,
  y2: number,
): number {
  const h0 = x1 - x0;
  const h1 = x2 - x1;
  // d3: `h0 || (h1 < 0 && -0)` -- `-0` is itself falsy, so `h0 !== 0` mirrors
  // the `||`'s truthiness test exactly (true for +0 and -0 alike).
  const denom0 = h0 !== 0 ? h0 : h1 < 0 ? -0 : 0;
  const denom1 = h1 !== 0 ? h1 : h0 < 0 ? -0 : 0;
  const s0 = (y1 - y0) / denom0;
  const s1 = (y2 - y1) / denom1;
  const p = (s0 * h1 + s1 * h0) / (h0 + h1);
  return (
    (monotoneSign(s0) + monotoneSign(s1)) *
      Math.min(Math.abs(s0), Math.abs(s1), 0.5 * Math.abs(p)) || 0
  );
}

/** The one-sided boundary slope at a curve's first or last point, ported
 *  verbatim from d3-shape's `slope2` (`curve/monotone.js`). */
function monotoneSlope2(x0: number, y0: number, x1: number, y1: number, t: number): number {
  const h = x1 - x0;
  return h !== 0 ? (3 * ((y1 - y0) / h) - t) / 2 : t;
}

/** The two interior Bezier control points for the segment (x0,y0) -> (x1,y1)
 *  given its start/end tangents, per the cubic-Hermite-as-Bezier identity
 *  d3-shape's `point` function (`curve/monotone.js`) cites (Wikipedia: Cubic
 *  Hermite spline, "Representations"). */
function monotoneControlPoints(
  x0: number,
  y0: number,
  x1: number,
  y1: number,
  t0: number,
  t1: number,
): { cp1x: number; cp1y: number; cp2x: number; cp2y: number } {
  const dx = (x1 - x0) / 3;
  return { cp1x: x0 + dx, cp1y: y0 + dx * t0, cp2x: x1 - dx, cp2y: y1 - dx * t1 };
}

/**
 * Build a smooth cubic bezier SVG path from a series of points, using a
 * monotone cubic interpolation (Fritsch-Carlson via Steffen's method) -- a
 * direct port of d3-shape's `curveMonotoneX` (`src/curve/monotone.js`), the
 * curve shadcn/Recharts mean by `type="monotone"` (rafters ruling
 * 2026-09-01, issue #2226). Unlike the Catmull-Rom conversion this replaces,
 * a monotone curve never overshoots the data: between any two points the
 * curve stays within their y-extent, so it never invents a local max/min the
 * data does not have.
 *
 * d3-shape drives this incrementally through a streaming context
 * (`moveTo`/`lineTo`/`bezierCurveTo`) fed one point at a time; this port
 * replays the SAME state machine (`lineStart`/`point`/`lineEnd`) over a
 * known-length array in one pass instead, using local variables in place of
 * the context's `this._x0`/`_x1`/`_t0`/`_point` fields. The arithmetic
 * (`monotoneSlope3`/`monotoneSlope2`/`monotoneControlPoints` above) is
 * unchanged from the source; only the driving loop differs. A reference test
 * (`graph.reference.test.ts`) diffs this against d3-shape's own
 * `line().curve(curveMonotoneX)` output.
 */
export function smoothPath(points: ReadonlyArray<{ x: number; y: number }>): string {
  // d3-shape's `MonotoneX.point` ignores a point coincident with the
  // immediately preceding one ("if (x === this._x1 && y === this._y1)
  // return;") before its state machine ever sees it -- deduped here, once,
  // rather than inside the loop below.
  const pts: { x: number; y: number }[] = [];
  for (const p of points) {
    const prev = pts[pts.length - 1];
    if (!prev || prev.x !== p.x || prev.y !== p.y) pts.push({ x: p.x, y: p.y });
  }

  if (pts.length === 0) return '';
  if (pts.length < 3) return linePath(pts); // d3 lineEnd case 1/2: M only, or M + one lineTo.

  let d = '';
  // Mirrors d3's this._x0/_y0 (two points back) and this._x1/_y1 (one point
  // back); NaN until enough points have been seen to mean anything.
  let x0 = Number.NaN;
  let y0 = Number.NaN;
  let x1 = Number.NaN;
  let y1 = Number.NaN;
  // Mirrors this._t0: the tangent at (x1,y1), carried into the NEXT
  // segment's bezier as its start tangent.
  let t0 = Number.NaN;

  // Draws the bezier for the segment ending at the CURRENT (x1,y1), starting
  // at the CURRENT (x0,y0) -- called before those are advanced for the point
  // that triggered it, same order d3's `point` calls its own `point()` before
  // its trailer reassigns `_x0`/`_x1`.
  const emitBezier = (startTangent: number, endTangent: number): void => {
    const { cp1x, cp1y, cp2x, cp2y } = monotoneControlPoints(
      x0,
      y0,
      x1,
      y1,
      startTangent,
      endTangent,
    );
    d += ` C ${cp1x} ${cp1y}, ${cp2x} ${cp2y}, ${x1} ${y1}`;
  };

  for (const [i, p] of pts.entries()) {
    if (i === 0) {
      d = `M ${p.x} ${p.y}`;
    } else if (i === 1) {
      // Second point: recorded only, same as d3's `_point === 1` case -- a
      // segment needs a third point before Steffen's formula has neighbors.
    } else if (i === 2) {
      // Third point: (x0,y0)/(x1,y1) are p0/p1. slope3 estimates the tangent
      // AT p1 from its neighbors p0/p2; the segment p0->p1 has no point
      // before p0, so its start tangent falls back to the one-sided
      // boundary formula (slope2), same as d3's `_point === 2` case.
      const t1 = monotoneSlope3(x0, y0, x1, y1, p.x, p.y);
      emitBezier(monotoneSlope2(x0, y0, x1, y1, t1), t1);
      t0 = t1;
    } else {
      // Interior points: (x0,y0)/(x1,y1) are the PREVIOUS pair; t0 is the
      // tangent already computed at (x1,y1) on a prior iteration. slope3
      // estimates the tangent at the new point p, closing the segment
      // between the previous pair with both of its now-known tangents, same
      // as d3's default case.
      const t1 = monotoneSlope3(x0, y0, x1, y1, p.x, p.y);
      emitBezier(t0, t1);
      t0 = t1;
    }
    x0 = x1;
    y0 = y1;
    x1 = p.x;
    y1 = p.y;
  }

  // d3's `lineEnd` case 3: the final segment ((x0,y0) -> (x1,y1), now the
  // LAST pair) uses the last computed tangent as its start and the one-sided
  // boundary formula as its end, mirroring the case-2 start above.
  emitBezier(t0, monotoneSlope2(x0, y0, x1, y1, t0));

  return d;
}

/**
 * Build an SVG arc path between two angles.
 * Angles in degrees, 0 = right (3 o'clock), counterclockwise.
 */
export function arcPath(
  cx: number,
  cy: number,
  radius: number,
  startAngle: number,
  endAngle: number,
): string {
  const start = polarToCartesian(cx, cy, radius, startAngle);
  const end = polarToCartesian(cx, cy, radius, endAngle);

  let sweep = endAngle - startAngle;
  if (sweep < 0) sweep += 360;
  const largeArc = sweep > 180 ? 1 : 0;

  return [
    `M ${start.x} ${start.y}`,
    `A ${radius} ${radius} 0 ${largeArc} 0 ${end.x} ${end.y}`,
  ].join(' ');
}

/**
 * Build a closed area SVG path: the line path closed back along a baseline.
 */
export function areaPath(
  points: { x: number; y: number }[],
  baseline: number,
  smooth?: boolean,
): string {
  if (points.length === 0) return '';

  const topPath = smooth ? smoothPath(points) : linePath(points);
  const last = points[points.length - 1] as { x: number; y: number };
  const first = points[0] as { x: number; y: number };

  return `${topPath} L ${last.x} ${baseline} L ${first.x} ${baseline} Z`;
}

/**
 * Build a full circle (or ring outline) as two semicircle arcs.
 * A single 360-degree arc has coincident endpoints, which the SVG spec drops as
 * a zero-length segment, so d3-shape (and this) splits it in two. sweepFlag 1 is
 * clockwise, 0 counterclockwise (used to cut the donut hole via winding).
 */
function circleArcs(cx: number, cy: number, r: number, sweepFlag: 0 | 1): string {
  const top = radialToCartesian(cx, cy, r, 0);
  const bottom = radialToCartesian(cx, cy, r, 180);
  return [
    `M ${top.x} ${top.y}`,
    `A ${r} ${r} 0 1 ${sweepFlag} ${bottom.x} ${bottom.y}`,
    `A ${r} ${r} 0 1 ${sweepFlag} ${top.x} ${top.y}`,
    'Z',
  ].join(' ');
}

/**
 * Build a filled pie/donut slice SVG path.
 * Angles in degrees, 0 = top (12 o'clock), positive = clockwise -- standard chart
 * convention. Matches d3-shape arc: the sweep is the absolute angular distance
 * (never the 360-minus complement), and the direction follows the angle sign --
 * endAngle > startAngle draws clockwise, endAngle < startAngle draws the short
 * arc counterclockwise.
 */
export function slicePath(
  cx: number,
  cy: number,
  outerRadius: number,
  innerRadius: number,
  startAngle: number,
  endAngle: number,
): string {
  const sweep = Math.abs(endAngle - startAngle);
  const clockwise = endAngle > startAngle;

  // Full circle: one 360-degree arc has coincident endpoints (dropped by the SVG
  // spec), so emit two semicircle arcs. Donut = outer circle + inner circle with
  // the reversed winding, cutting the hole; the winding direction still follows
  // the requested sweep direction.
  if (sweep >= 360 - 1e-9) {
    const outerFlag = clockwise ? 1 : 0;
    const innerFlag = clockwise ? 0 : 1;
    const outer = circleArcs(cx, cy, outerRadius, outerFlag);
    return innerRadius <= 0 ? outer : `${outer} ${circleArcs(cx, cy, innerRadius, innerFlag)}`;
  }

  const outerStart = radialToCartesian(cx, cy, outerRadius, startAngle);
  const outerEnd = radialToCartesian(cx, cy, outerRadius, endAngle);
  const largeArc = sweep > 180 ? 1 : 0;
  const outerSweepFlag = clockwise ? 1 : 0;
  const innerSweepFlag = clockwise ? 0 : 1;

  if (innerRadius <= 0) {
    return [
      `M ${cx} ${cy}`,
      `L ${outerStart.x} ${outerStart.y}`,
      `A ${outerRadius} ${outerRadius} 0 ${largeArc} ${outerSweepFlag} ${outerEnd.x} ${outerEnd.y}`,
      'Z',
    ].join(' ');
  }

  const innerStart = radialToCartesian(cx, cy, innerRadius, startAngle);
  const innerEnd = radialToCartesian(cx, cy, innerRadius, endAngle);

  return [
    `M ${outerStart.x} ${outerStart.y}`,
    `A ${outerRadius} ${outerRadius} 0 ${largeArc} ${outerSweepFlag} ${outerEnd.x} ${outerEnd.y}`,
    `L ${innerEnd.x} ${innerEnd.y}`,
    `A ${innerRadius} ${innerRadius} 0 ${largeArc} ${innerSweepFlag} ${innerStart.x} ${innerStart.y}`,
    'Z',
  ].join(' ');
}

/**
 * Convert a value on a radial axis to cartesian coordinates.
 * Used for radar/radial charts. Angle 0 = top, clockwise.
 */
export function radialToCartesian(
  cx: number,
  cy: number,
  radius: number,
  angleDeg: number,
): { x: number; y: number } {
  const angleRad = ((angleDeg - 90) * Math.PI) / 180;
  return {
    x: cx + radius * Math.cos(angleRad),
    y: cy + radius * Math.sin(angleRad),
  };
}

/**
 * Build a closed polygon path for radar charts.
 * Takes values at equally-spaced angles from 0 (top), clockwise.
 */
export function radarPath(cx: number, cy: number, values: number[], maxRadius: number): string {
  if (values.length === 0) return '';

  const angleStep = 360 / values.length;
  const points = values.map((v, i) => {
    const angle = i * angleStep;
    const r = v * maxRadius;
    return radialToCartesian(cx, cy, r, angle);
  });

  return linePath(points) + ' Z';
}

/**
 * Observe a container's content-box size, calling back with { width, height }.
 * Fires once on observe and on every subsequent resize (ResizeObserver's own
 * cadence). Returns a cleanup that disconnects. SSR-safe: a no-op cleanup when
 * ResizeObserver is absent. Signature matches the #2223 pinned interface.
 */
export function observeResize(
  el: HTMLElement,
  onResize: (size: { width: number; height: number }) => void,
): () => void {
  if (typeof ResizeObserver === 'undefined') {
    return () => {};
  }

  const observer = new ResizeObserver((entries) => {
    const entry = entries[0];
    if (!entry) return;
    const { width, height } = entry.contentRect;
    onResize({ width, height });
  });

  observer.observe(el);

  return () => {
    observer.disconnect();
  };
}
