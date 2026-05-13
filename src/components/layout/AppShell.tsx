import { type ReactNode } from "react";
import { TitleBar } from "./TitleBar";
import { Sidebar } from "./Sidebar";

interface Props {
  children: ReactNode;
  onRefresh: () => void;
  showMaximize?: boolean;
}

export function AppShell({ children, onRefresh, showMaximize = true }: Props) {
  return (
    <div
      style={{
        width: "100vw",
        height: "100vh",
        display: "flex",
        flexDirection: "column",
        background: "var(--mm-bg)",
        overflow: "hidden",
      }}
    >
      <TitleBar showMaximize={showMaximize} />
      <div style={{ display: "flex", flex: 1, minHeight: 0 }}>
        <Sidebar onRefresh={onRefresh} />
        <div
          style={{
            flex: 1,
            display: "flex",
            flexDirection: "column",
            minWidth: 0,
            minHeight: 0,
          }}
        >
          {children}
        </div>
      </div>
    </div>
  );
}
