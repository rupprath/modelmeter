import { useEffect, useState } from "react";
import { type SyncIndicator as Indicator } from "../../lib/types";
import { relativeTime } from "../../lib/time";

interface Props {
  indicator: Indicator;
  lastTickAt: number | null;
  paused: boolean;
  hasProviders?: boolean;
}

export function SyncIndicator({ indicator, lastTickAt, paused, hasProviders = true }: Props) {
  const [, setTick] = useState(0);
  useEffect(() => {
    const id = setInterval(() => setTick((t) => t + 1), 30_000);
    return () => clearInterval(id);
  }, []);

  let cls = "mm-sync";
  let label: string;

  if (!hasProviders) {
    label = "no providers configured";
  } else if (paused) {
    cls += " warn";
    label = "sync paused";
  } else if (indicator === "spinner") {
    cls += " syncing";
    label = "syncing now…";
  } else if (indicator === "amber") {
    cls += " warn";
    label = lastTickAt ? `stale · ${relativeTime(lastTickAt)}` : "stale";
  } else if (indicator === "grey") {
    label = "sync paused";
  } else {
    // green
    cls += " ok";
    label = lastTickAt ? `synced ${relativeTime(lastTickAt)}` : "synced";
  }

  return (
    <div className={cls}>
      <span className="mm-dot" />
      <span>{label}</span>
    </div>
  );
}
