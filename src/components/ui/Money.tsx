interface MoneyProps {
  value: number;
  big?: boolean;
  mutedCents?: boolean;
}

export function Money({ value, big = false, mutedCents = true }: MoneyProps) {
  const sign = value < 0 ? "-" : "";
  const abs = Math.abs(value);
  const dollars = Math.floor(abs).toLocaleString();
  const cents = (abs - Math.floor(abs)).toFixed(2).slice(2);
  return (
    <span
      className="mm-money mm-num"
      style={big ? { fontSize: 36, fontWeight: 700, letterSpacing: "-0.02em" } : undefined}
    >
      <span>$</span>
      {sign}
      {dollars}
      <span className={mutedCents ? "mm-money-cents" : undefined}>.{cents}</span>
    </span>
  );
}

