import { useLocation, useNavigate } from "react-router-dom";

// 17×17 stroked SVGs matching the design spec (1.3–1.5 stroke-width)
function IcoDashboard() {
  return (
    <svg viewBox="0 0 16 16" width="17" height="17" fill="none" stroke="currentColor" strokeWidth="1.4">
      <rect x="2.5" y="2.5" width="5" height="5" rx="1" />
      <rect x="8.5" y="2.5" width="5" height="5" rx="1" />
      <rect x="2.5" y="8.5" width="5" height="5" rx="1" />
      <rect x="8.5" y="8.5" width="5" height="5" rx="1" />
    </svg>
  );
}

function IcoPlus() {
  return (
    <svg viewBox="0 0 16 16" width="17" height="17" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round">
      <line x1="8" y1="3" x2="8" y2="13" />
      <line x1="3" y1="8" x2="13" y2="8" />
    </svg>
  );
}

function IcoRefresh() {
  return (
    <svg viewBox="0 0 16 16" width="17" height="17" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
      <path d="M14 8a6 6 0 1 1-1.76-4.24" />
      <path d="M14 2v4h-4" />
    </svg>
  );
}

function IcoCog() {
  return (
    <svg viewBox="0 0 16 16" width="17" height="17" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round">
      <path d="M12.8 6.7L14.8 6.2L14.8 9.8L12.8 9.3L11.5 11.5L13 13L9.8 14.8L9.3 12.8L6.7 12.8L6.2 14.8L3.1 13L4.5 11.5L3.2 9.3L1.2 9.8L1.2 6.2L3.2 6.7L4.5 4.5L3.1 3.1L6.2 1.2L6.7 3.2L9.3 3.2L9.8 1.2L13 3.1L11.5 4.5Z" />
      <circle cx="8" cy="8" r="2.5" />
    </svg>
  );
}

interface RailBtnProps {
  active?: boolean;
  title: string;
  onClick: () => void;
  children: React.ReactNode;
}

function RailBtn({ active, title, onClick, children }: RailBtnProps) {
  return (
    <button
      className={`sv-side-btn${active ? " active" : ""}`}
      title={title}
      onClick={onClick}
    >
      {children}
    </button>
  );
}

interface Props {
  onRefresh: () => void;
}

export function Sidebar({ onRefresh }: Props) {
  const location = useLocation();
  const navigate = useNavigate();

  const isDashboard = location.pathname === "/";
  const isSettings = location.pathname === "/settings";

  return (
    <nav className="sv-side" aria-label="Main navigation">
      <RailBtn active={isDashboard} title="Dashboard" onClick={() => navigate("/")}>
        <IcoDashboard />
      </RailBtn>
      <RailBtn title="Add provider" onClick={() => navigate("/providers")}>
        <IcoPlus />
      </RailBtn>
      <RailBtn title="Sync now" onClick={onRefresh}>
        <IcoRefresh />
      </RailBtn>

      <div className="sv-side-spacer" />

      <RailBtn active={isSettings} title="Settings" onClick={() => navigate("/settings")}>
        <IcoCog />
      </RailBtn>
    </nav>
  );
}
