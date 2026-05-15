interface MoneyProps {
  value: number;
  big?: boolean;
  currency?: string;
}

export function Money({ value, big = false, currency = 'USD' }: MoneyProps) {
  const formatted = new Intl.NumberFormat(undefined, {
    style: 'currency',
    currency,
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  }).format(value);

  return (
    <span
      className="mm-money mm-num"
      style={big ? { fontSize: 36, fontWeight: 700, letterSpacing: '-0.02em' } : undefined}
    >
      {formatted}
    </span>
  );
}
