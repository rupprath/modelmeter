import { useRef, useState } from "react";

interface HoverState {
  i: number;
  x: number;
  value: number;
}

interface Props {
  data: number[];
  color?: string;
  height?: number;
  padTop?: number;
  currency?: string;
  /**
   * Optional per-bar labels (length must match `data`). When provided, used
   * for both the X-axis ticks and the hover tooltip's prefix. When omitted,
   * the bars are labelled as days ago — "today", "1d ago", "2d", etc.
   */
  labels?: string[];
}

export function DailyBars({ data, color = "#1a1a1a", height = 84, padTop = 10, currency = 'USD', labels }: Props) {
  const svgRef = useRef<SVGSVGElement>(null);
  const [hover, setHover] = useState<HoverState | null>(null);

  const WIDTH = 600;
  const padRight = 16;
  const padBottom = 12;
  const innerH = height - padTop - padBottom;
  const n = data.length;
  const max = Math.max(...data, 1);
  const gap = 2;
  const bw = (WIDTH - gap * (n - 1)) / n;

  const ticks = n <= 4
    ? Array.from({ length: n }, (_, i) => i)
    : [0, Math.floor(n / 3), Math.floor((2 * n) / 3), n - 1];
  const tickLabels = ticks.map((i) => {
    if (labels) return labels[i] ?? "";
    const daysAgo = n - 1 - i;
    return daysAgo === 0 ? "today" : `${daysAgo}d`;
  });
  const gridYs = [0.33, 0.66].map((f) => padTop + innerH * f);

  function handleMove(e: React.MouseEvent<SVGSVGElement>) {
    const svg = svgRef.current;
    if (!svg) return;
    const rect = svg.getBoundingClientRect();
    const xPx = e.clientX - rect.left;
    const xViewbox = (xPx / rect.width) * WIDTH;
    let idx = Math.floor(xViewbox / (bw + gap));
    if (idx < 0) idx = 0;
    if (idx > n - 1) idx = n - 1;
    const cxView = idx * (bw + gap) + bw / 2;
    const cxPx = (cxView / WIDTH) * rect.width;
    setHover({ i: idx, x: cxPx, value: data[idx] });
  }

  const tooltipLabel = (i: number): string => {
    if (labels) return labels[i] ?? "";
    const d = n - 1 - i;
    return d === 0 ? "today" : `${d}d ago`;
  };

  return (
    <div className="sv-chart-wrap" onMouseLeave={() => setHover(null)}>
      <svg
        ref={svgRef}
        viewBox={`0 0 ${WIDTH + padRight} ${height}`}
        width="100%"
        height={height}
        preserveAspectRatio="none"
        style={{ display: "block" }}
        onMouseMove={handleMove}
      >
        {gridYs.map((y, i) => (
          <line
            key={i}
            x1={0}
            x2={WIDTH + padRight}
            y1={y}
            y2={y}
            stroke="var(--mm-divider)"
            strokeDasharray="2 3"
          />
        ))}

        {data.map((v, i) => {
          const h = (v / max) * innerH;
          const x = i * (bw + gap);
          const y = padTop + innerH - h;
          const isLast = i === n - 1;
          const isHover = hover?.i === i;
          return (
            <rect
              key={i}
              x={x.toFixed(2)}
              y={y.toFixed(2)}
              width={Math.max(bw, 1).toFixed(2)}
              height={Math.max(h, 1).toFixed(2)}
              fill={color}
              opacity={isHover || isLast ? 1 : 0.78}
              rx={Math.min(1.5, bw / 2)}
            />
          );
        })}

        {ticks.map((t, i) => (
          <text
            key={i}
            x={t * (bw + gap) + bw / 2}
            y={height - 2}
            textAnchor="middle"
            fontSize="10"
            fill="var(--mm-text-4)"
            style={{ fontFamily: "var(--mm-font-text)" }}
          >
            {tickLabels[i]}
          </text>
        ))}
      </svg>

      {hover && (
        <div
          className="sv-tooltip"
          style={{ left: hover.x, top: padTop + 2 }}
        >
          {tooltipLabel(hover.i)} · {new Intl.NumberFormat(undefined, { style: 'currency', currency, minimumFractionDigits: 2, maximumFractionDigits: 2 }).format(hover.value)}
        </div>
      )}
    </div>
  );
}
