import { type ReactNode } from "react";
import { RefreshCw } from "lucide-react";
import { SyncIndicator } from "../ui/SyncIndicator";
import { type SyncStatus } from "../../lib/types";

interface Props {
  title: string;
  syncStatus: SyncStatus | null;
  onRefresh?: () => void;
  right?: ReactNode;
  hasProviders?: boolean;
}

const IDLE_SYNC: SyncStatus = {
  paused: false,
  last_tick_at: null,
  indicator: "spinner",
  providers: {},
};

export function Toolbar({ title, syncStatus, onRefresh, right, hasProviders = true }: Props) {
  const status = syncStatus ?? IDLE_SYNC;
  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        gap: 12,
        padding: "0 16px",
        height: 46,
        flex: "0 0 46px",
        borderBottom: "1px solid var(--mm-divider)",
        background: "var(--mm-chrome)",
      }}
    >
      <div style={{ fontSize: 14, fontWeight: 600, letterSpacing: "-0.01em" }}>{title}</div>

      <SyncIndicator
        indicator={status.indicator}
        lastTickAt={status.last_tick_at}
        paused={status.paused}
        hasProviders={hasProviders}
      />

      <div style={{ marginLeft: "auto", display: "flex", alignItems: "center", gap: 6 }}>
        {right}
        <button className="mm-btn ghost" onClick={onRefresh} title="Refresh">
          <RefreshCw size={13} />
          Refresh
        </button>
      </div>
    </div>
  );
}
