import { useEffect, useState } from "react";
import { Server, Plus, Trash2, CheckCircle, XCircle, Loader } from "lucide-react";
import { Toolbar } from "../components/layout/Toolbar";
import { addProvider, removeProvider, validateProviderKey } from "../lib/tauri";
import { type Provider, type ProviderKindMeta } from "../lib/types";

interface Props {
  providers: Provider[];
  onProvidersChanged: () => void;
  providerKinds: ProviderKindMeta[];
}

// Tauri 2 invoke errors are plain objects like { message: string }, not Error instances.
function tauriErrMsg(e: unknown): string {
  if (typeof e === "string") return e;
  if (e && typeof e === "object" && "message" in e) return String((e as { message: unknown }).message);
  return JSON.stringify(e);
}

// ── Add provider modal ─────────────────────────────────────────────────────

type ValidationState =
  | { kind: "idle" }
  | { kind: "validating" }
  | { kind: "valid" }
  | { kind: "invalid"; reason: string; adminKeyUrl?: string };

function AddProviderModal({
  onClose,
  onAdded,
  providerKinds,
}: {
  onClose: () => void;
  onAdded: () => void;
  providerKinds: ProviderKindMeta[];
}) {
  const [providerType, setProviderType] = useState<string>(providerKinds[0]?.slug ?? "");
  const [displayName, setDisplayName] = useState("");
  const [key, setKey] = useState("");
  const [validation, setValidation] = useState<ValidationState>({ kind: "idle" });
  const [adding, setAdding] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const selectedKind = providerKinds.find((k) => k.slug === providerType);
  const keyRequired = selectedKind?.key_required ?? true;

  // For providers that need no key, auto-validate as soon as the type is selected.
  useEffect(() => {
    if (!keyRequired) {
      setValidation({ kind: "validating" });
      validateProviderKey(providerType, "")
        .then((result) => {
          if (result.status === "valid") {
            setValidation({ kind: "valid" });
          } else if (result.status === "invalid") {
            setValidation({ kind: "invalid", reason: result.reason });
          } else {
            setValidation({ kind: "invalid", reason: `Insufficient permission: ${result.hint}` });
          }
        })
        .catch((e) => setValidation({ kind: "invalid", reason: tauriErrMsg(e) }));
    }
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [providerType, keyRequired]);

  const canValidate = keyRequired && key.trim().length > 0 && validation.kind !== "validating";
  const canAdd =
    displayName.trim().length > 0 &&
    (!keyRequired || key.trim().length > 0) &&
    validation.kind === "valid" &&
    !adding;

  const handleValidate = async () => {
    setValidation({ kind: "validating" });
    setError(null);
    try {
      const result = await validateProviderKey(providerType, key.trim());
      if (result.status === "valid") {
        setValidation({ kind: "valid" });
      } else if (result.status === "invalid") {
        setValidation({ kind: "invalid", reason: result.reason });
      } else {
        const meta = providerKinds.find((k) => k.slug === providerType);
        setValidation({
          kind: "invalid",
          reason: `Insufficient permission: ${result.hint}`,
          adminKeyUrl: meta?.key_docs_url ?? undefined,
        });
      }
    } catch (e) {
      setValidation({ kind: "invalid", reason: tauriErrMsg(e) });
    }
  };

  const handleAdd = async () => {
    setAdding(true);
    setError(null);
    try {
      await addProvider(providerType, displayName.trim(), key.trim());
      onAdded();
      onClose();
    } catch (e) {
      setError(tauriErrMsg(e));
      setAdding(false);
    }
  };

  const resetValidation = () => setValidation({ kind: "idle" });

  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(0,0,0,0.45)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        zIndex: 1000,
      }}
      onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}
    >
      <div
        className="mm-card"
        style={{
          width: 380,
          padding: 20,
          display: "flex",
          flexDirection: "column",
          gap: 14,
        }}
      >
        <div style={{ fontWeight: 600, fontSize: 13 }}>Add provider</div>

        <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
          <label style={{ fontSize: 11, color: "var(--mm-text-3)", fontWeight: 500 }}>Provider</label>
          <select
            className="mm-input"
            value={providerType}
            onChange={(e) => {
              setProviderType(e.target.value);
              setKey("");
              resetValidation();
            }}
            style={{ height: 30, fontSize: 12, appearance: "auto" }}
          >
            {providerKinds.map((k) => (
              <option key={k.slug} value={k.slug}>
                {k.slug === "claude_code"
                  ? `${k.display_name} (requires Claude Code installation)`
                  : k.display_name}
              </option>
            ))}
          </select>
        </div>

        <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
          <label style={{ fontSize: 11, color: "var(--mm-text-3)", fontWeight: 500 }}>Display name</label>
          <input
            className="mm-input"
            type="text"
            placeholder="e.g. My OpenAI key"
            value={displayName}
            onChange={(e) => setDisplayName(e.target.value)}
            style={{ height: 30, fontSize: 12 }}
          />
        </div>

        {keyRequired ? (
          <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
            <label style={{ fontSize: 11, color: "var(--mm-text-3)", fontWeight: 500 }}>
              {selectedKind?.key_label ?? "API key"}
            </label>
            <div style={{ display: "flex", gap: 6 }}>
              <input
                className="mm-input"
                type={selectedKind?.key_is_secret ? "password" : "text"}
                placeholder="sk-..."
                value={key}
                onChange={(e) => { setKey(e.target.value); resetValidation(); }}
                style={{ flex: 1, height: 30, fontSize: 12 }}
              />
              <button
                className="mm-btn ghost"
                style={{ height: 30, whiteSpace: "nowrap", fontSize: 11 }}
                disabled={!canValidate}
                onClick={handleValidate}
              >
                {validation.kind === "validating" ? (
                  <Loader size={12} style={{ animation: "mm-spin 1s linear infinite" }} />
                ) : "Validate"}
              </button>
            </div>
            {validation.kind === "valid" && (
              <div style={{ display: "flex", alignItems: "center", gap: 5, fontSize: 11, color: "var(--mm-ok, #22c55e)" }}>
                <CheckCircle size={12} /> Key is valid
              </div>
            )}
            {validation.kind === "invalid" && (
              <div style={{ display: "flex", flexDirection: "column", gap: 4, fontSize: 11, color: "var(--mm-err)" }}>
                <div style={{ display: "flex", alignItems: "center", gap: 5 }}>
                  <XCircle size={12} /> {validation.reason}
                </div>
                {validation.adminKeyUrl && (
                  <div style={{ paddingLeft: 17, color: "var(--mm-text-3)" }}>
                    Get an admin key at:{" "}
                    <span style={{ color: "var(--mm-accent)", userSelect: "all" }}>
                      {validation.adminKeyUrl}
                    </span>
                  </div>
                )}
              </div>
            )}
          </div>
        ) : (
          <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
            {validation.kind === "validating" && (
              <div style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 11, color: "var(--mm-text-3)" }}>
                <Loader size={12} style={{ animation: "mm-spin 1s linear infinite" }} />
                Checking for Claude Code credentials…
              </div>
            )}
            {validation.kind === "valid" && (
              <div style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 11, color: "var(--mm-ok, #22c55e)" }}>
                <CheckCircle size={12} /> Claude Code credentials found — you're ready to add this provider
              </div>
            )}
            {validation.kind === "invalid" && (
              <div style={{ display: "flex", alignItems: "flex-start", gap: 6, fontSize: 11, color: "var(--mm-err)" }}>
                <XCircle size={12} style={{ flexShrink: 0, marginTop: 1 }} />
                <span>
                  {validation.reason}
                  <span style={{ display: "block", marginTop: 4, color: "var(--mm-text-3)" }}>
                    Open Claude Code and sign in, then come back here.
                  </span>
                </span>
              </div>
            )}
          </div>
        )}

        {error && (
          <div style={{ fontSize: 11, color: "var(--mm-err)", padding: "6px 8px", background: "color-mix(in srgb, var(--mm-err) 10%, transparent)", borderRadius: 4 }}>
            {error}
          </div>
        )}

        <div style={{ display: "flex", justifyContent: "flex-end", gap: 8, marginTop: 2 }}>
          <button className="mm-btn ghost" onClick={onClose} disabled={adding}>Cancel</button>
          <button className="mm-btn primary" onClick={handleAdd} disabled={!canAdd}>
            {adding ? "Adding…" : "Add provider"}
          </button>
        </div>
      </div>
    </div>
  );
}

// ── Provider row ───────────────────────────────────────────────────────────

function ProviderRow({
  provider,
  meta,
  onRemove,
}: {
  provider: Provider;
  meta: ProviderKindMeta | undefined;
  onRemove: () => void;
}) {
  const [confirming, setConfirming] = useState(false);
  const [removing, setRemoving] = useState(false);

  const displayName = meta?.display_name ?? provider.provider_type;
  const short = meta?.short ?? provider.provider_type[0]?.toUpperCase() ?? "?";
  const color = meta?.color ?? "#666";

  const handleRemove = async () => {
    setRemoving(true);
    try {
      await removeProvider(provider.id);
      onRemove();
    } catch {
      setRemoving(false);
      setConfirming(false);
    }
  };

  const statusColor =
    provider.last_sync_status === "ok"
      ? "var(--mm-ok, #22c55e)"
      : provider.last_sync_status === "failed"
      ? "var(--mm-err)"
      : "var(--mm-text-4)";

  return (
    <div
      className="mm-card"
      style={{
        display: "flex",
        alignItems: "center",
        padding: "10px 14px",
        gap: 12,
      }}
    >
      <div
        style={{
          width: 28,
          height: 28,
          borderRadius: 6,
          background: color,
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          fontSize: 11,
          fontWeight: 700,
          color: "#fff",
          flexShrink: 0,
        }}
      >
        {short}
      </div>

      <div style={{ flex: 1, minWidth: 0 }}>
        <div style={{ fontSize: 12, fontWeight: 600, color: "var(--mm-text)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
          {provider.display_name}
        </div>
        <div style={{ fontSize: 11, color: "var(--mm-text-3)", marginTop: 1 }}>
          {displayName}
          <span style={{ marginLeft: 8, color: statusColor }}>
            {provider.last_sync_status === "ok"
              ? "Synced"
              : provider.last_sync_status === "failed"
              ? "Sync failed"
              : "Never synced"}
          </span>
        </div>
      </div>

      {confirming ? (
        <div style={{ display: "flex", gap: 6, alignItems: "center" }}>
          <span style={{ fontSize: 11, color: "var(--mm-text-3)" }}>Remove?</span>
          <button
            className="mm-btn ghost"
            style={{ height: 26, fontSize: 11 }}
            onClick={() => setConfirming(false)}
            disabled={removing}
          >
            Cancel
          </button>
          <button
            className="mm-btn"
            style={{ height: 26, fontSize: 11, background: "var(--mm-err)", color: "#fff", border: "none" }}
            onClick={handleRemove}
            disabled={removing}
          >
            {removing ? "Removing…" : "Remove"}
          </button>
        </div>
      ) : (
        <button
          className="mm-iconbtn"
          title="Remove provider"
          onClick={() => setConfirming(true)}
          style={{ color: "var(--mm-text-4)" }}
        >
          <Trash2 size={13} />
        </button>
      )}
    </div>
  );
}

// ── Providers page ─────────────────────────────────────────────────────────

export function Providers({ providers, onProvidersChanged, providerKinds }: Props) {
  const [showAdd, setShowAdd] = useState(false);

  return (
    <>
      <Toolbar
        title="Providers"
        syncStatus={null}
        hasProviders={providers.length > 0}
        onRefresh={() => {}}
        right={
          <button className="mm-btn primary" onClick={() => setShowAdd(true)}>
            <Plus size={13} />
            Add provider
          </button>
        }
      />

      <div style={{ flex: 1, overflow: "auto", padding: "var(--mm-pad)", display: "flex", flexDirection: "column", gap: 8 }}>
        {providers.length === 0 ? (
          <div
            style={{
              flex: 1,
              display: "flex",
              flexDirection: "column",
              alignItems: "center",
              justifyContent: "center",
              gap: 16,
              padding: 24,
              color: "var(--mm-text-4)",
            }}
          >
            <Server size={32} strokeWidth={1.5} />
            <div style={{ textAlign: "center" }}>
              <div style={{ fontWeight: 600, fontSize: 13, color: "var(--mm-text-3)", marginBottom: 6 }}>
                No providers yet
              </div>
              <div style={{ fontSize: 12, maxWidth: 260 }}>
                Add an OpenAI or Anthropic API key to start tracking usage and spend.
              </div>
            </div>
            <button className="mm-btn primary" onClick={() => setShowAdd(true)}>
              <Plus size={13} />
              Add provider
            </button>
          </div>
        ) : (
          providers.map((p) => (
            <ProviderRow
              key={p.id}
              provider={p}
              meta={providerKinds.find((k) => k.slug === p.provider_type)}
              onRemove={onProvidersChanged}
            />
          ))
        )}
      </div>

      {showAdd && (
        <AddProviderModal
          onClose={() => setShowAdd(false)}
          onAdded={onProvidersChanged}
          providerKinds={providerKinds}
        />
      )}
    </>
  );
}
