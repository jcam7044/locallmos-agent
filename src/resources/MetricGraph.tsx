export type GraphSeries = {
  label: string;
  color: string;
  values: Array<number | null>;
};

export function polylineSegments(
  values: Array<number | null>,
  width: number,
  height: number,
  maxValue: number,
) {
  const padded = [...Array(Math.max(0, 60 - values.length)).fill(null), ...values].slice(-60);
  const segments: string[] = [];
  let current: string[] = [];
  padded.forEach((value, index) => {
    if (value == null || !Number.isFinite(value)) {
      if (current.length > 1) segments.push(current.join(" "));
      current = [];
      return;
    }
    const x = (index / Math.max(1, padded.length - 1)) * width;
    const y = height - (Math.max(0, Math.min(value, maxValue)) / maxValue) * height;
    current.push(`${x.toFixed(2)},${y.toFixed(2)}`);
  });
  if (current.length > 1) segments.push(current.join(" "));
  return segments;
}

export function MetricGraph({
  label,
  series,
  max,
  height = 190,
}: {
  label: string;
  series: GraphSeries[];
  max?: number;
  height?: number;
}) {
  const observed = series.flatMap((item) => item.values).reduce<number>(
    (highest, value) => (value == null ? highest : Math.max(highest, value)),
    0,
  );
  const ceiling = Math.max(1, max ?? observed * 1.12);
  const width = 600;

  return (
    <div className="metric-graph">
      <svg
        viewBox={`0 0 ${width} ${height}`}
        preserveAspectRatio="none"
        role="img"
        aria-label={`${label}, last 60 seconds`}
      >
        {[0.25, 0.5, 0.75].map((position) => (
          <line
            key={position}
            x1="0"
            x2={width}
            y1={height * position}
            y2={height * position}
            className="graph-grid-line"
          />
        ))}
        {series.flatMap((item) =>
          polylineSegments(item.values, width, height, ceiling).map((points, index) => (
            <polyline
              key={`${item.label}-${index}`}
              points={points}
              fill="none"
              stroke={item.color}
              strokeWidth="2"
              vectorEffect="non-scaling-stroke"
            />
          )),
        )}
      </svg>
      <div className="graph-time"><span>60 seconds</span><span>Now</span></div>
    </div>
  );
}
