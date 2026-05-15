import { useEffect, useState } from "react";
import { type SyncIndicator as Indicator } from "../../lib/types";
import { relativeTime } from "../../lib/time";

interface Props {
  indicator: Indicator;
  lastSyncedAt: number | null;
  paused: boolean;
  hasProviders?: boolean;
}

export function SyncIndicator({ indicator, lastSyncedAt, paused, hasProviders = true }: Props) {
  const [, setTick] = useState(0);
  useEffect(() => {
    const id = setInterval(() => setTick((t) => t + 1), 60_000);
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
    label = lastSyncedAt ? `stale · ${relativeTime(lastSyncedAt)}` : "stale";
  } else if (indicator === "grey") {
    label = "sync paused";
  } else {
    // green
    cls += " ok";
    label = lastSyncedAt ? `synced ${relativeTime(lastSyncedAt)}` : "synced";
  }

  return (
    <div className={cls}>
      <span className="mm-dot" />
      <span>{label}</span>
    </div>
  );
}
