import { useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";

interface Props {
  showMaximize?: boolean;
  closeAction?: "hide" | "close";
}

export function TitleBar({ showMaximize = true, closeAction = "hide" }: Props) {
  const [maximized, setMaximized] = useState(false);

  useEffect(() => {
    const win = getCurrentWindow();
    let unlisten: (() => void) | null = null;

    const init = async () => {
      setMaximized(await win.isMaximized());
      unlisten = await win.onResized(async () => {
        setMaximized(await win.isMaximized());
      });
    };

    init().catch(() => {});

    return () => {
      unlisten?.();
    };
  }, []);

  const handleMinimize = () => getCurrentWindow().minimize().catch(() => {});
  const handleMaximize = () => getCurrentWindow().toggleMaximize().catch(() => {});
  const handleClose = () => {
    const win = getCurrentWindow();
    (closeAction === "close" ? win.close() : win.hide()).catch(() => {});
  };

  return (
    <div className="mm-titlebar" data-tauri-drag-region>
      <div className="mm-title">
        <div className="mm-title-icon" />
        <span>ModelMeter</span>
      </div>
      <div className="mm-wctrls">
        <button className="mm-wctrl" title="Minimize" aria-label="Minimize" onClick={handleMinimize}>
          <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
            <line x1="1" y1="5" x2="9" y2="5" stroke="currentColor" strokeWidth="1" />
          </svg>
        </button>
        {showMaximize && (
          <button className="mm-wctrl" title={maximized ? "Restore" : "Maximize"} aria-label={maximized ? "Restore" : "Maximize"} onClick={handleMaximize}>
            {maximized ? (
              <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
                <rect x="3" y="1" width="6" height="6" stroke="currentColor" strokeWidth="1" fill="none" />
                <path d="M1 3v6h6" stroke="currentColor" strokeWidth="1" fill="none" />
              </svg>
            ) : (
              <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
                <rect x="1.5" y="1.5" width="7" height="7" stroke="currentColor" strokeWidth="1" fill="none" />
              </svg>
            )}
          </button>
        )}
        <button className="mm-wctrl close" title="Close" aria-label="Close" onClick={handleClose}>
          <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
            <line x1="1" y1="1" x2="9" y2="9" stroke="currentColor" strokeWidth="1" />
            <line x1="9" y1="1" x2="1" y2="9" stroke="currentColor" strokeWidth="1" />
          </svg>
        </button>
      </div>
    </div>
  );
}
