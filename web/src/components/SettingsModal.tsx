import { FormEvent } from "react";
import { Modal } from "./Modal";
import type { ConfigPayload, ProviderInfo } from "../types";

type SettingsModalProps = {
  config: ConfigPayload;
  selectedProvider: ProviderInfo | null;
  configApiKey: string;
  setConfigApiKey: (value: string) => void;
  setConfig: React.Dispatch<React.SetStateAction<ConfigPayload | null>>;
  onClose: () => void;
  onSave: () => Promise<void>;
};

export function SettingsModal({
  config,
  selectedProvider,
  configApiKey,
  setConfigApiKey,
  setConfig,
  onClose,
  onSave,
}: SettingsModalProps) {
  function handleSubmit(event: FormEvent) {
    event.preventDefault();
    void onSave();
  }

  return (
    <Modal onClose={onClose} labelledBy="modal-title">
      <div className="flex justify-between gap-3 px-6 pt-6 pb-2 shrink-0">
        <div>
          <h3 id="modal-title" className="m-0 text-lg">
            Runtime Config
          </h3>
          <p className="mt-1 text-sm text-muted">{config.config_path}</p>
        </div>
        <button
          className="icon-button"
          onClick={onClose}
          aria-label="Close modal"
        >
          ×
        </button>
      </div>

      <form className="config-form" onSubmit={handleSubmit}>
        <div className="config-section-title">Default LLM</div>
        <label>
          <span>Provider</span>
          <select
            value={config.default_provider}
            onChange={(event) => {
              const providerId = event.target.value;
              const provider = config.providers.find(
                (item) => item.id === providerId,
              );
              setConfigApiKey("");
              setConfig({
                ...config,
                default_provider: providerId,
                default_model: provider?.default_model || config.default_model,
                has_api_key: provider?.has_api_key || false,
              });
            }}
          >
            {config.providers.map((provider) => (
              <option key={provider.id} value={provider.id}>
                {provider.label}
              </option>
            ))}
          </select>
        </label>
        <label>
          <span>Model</span>
          <input
            list="provider-models"
            value={config.default_model}
            onChange={(event) =>
              setConfig({ ...config, default_model: event.target.value })
            }
          />
          <datalist id="provider-models">
            {(selectedProvider?.models || []).map((model) => (
              <option key={model} value={model} />
            ))}
          </datalist>
        </label>
        <label>
          <span>API Key</span>
          <div className="api-key-row">
            <input
              type="password"
              value={configApiKey}
              placeholder={
                selectedProvider?.has_api_key
                  ? "Configured. Enter to replace."
                  : "Enter API key"
              }
              onChange={(event) => setConfigApiKey(event.target.value)}
            />
            {selectedProvider?.has_api_key ? (
              <button
                type="button"
                className="secondary-button api-key-clear"
                onClick={() => setConfigApiKey("*CLEAR*")}
              >
                Clear
              </button>
            ) : null}
          </div>
        </label>

        <div className="config-section-title">Web Server</div>
        <label className="checkbox-row">
          <input
            type="checkbox"
            checked={config.web_enabled}
            onChange={(event) =>
              setConfig({ ...config, web_enabled: event.target.checked })
            }
          />
          <span>Enable</span>
        </label>
        <div className="grid-two">
          <label>
            <span>Host</span>
            <input
              value={config.web_host}
              onChange={(event) =>
                setConfig({ ...config, web_host: event.target.value })
              }
            />
          </label>
          <label>
            <span>Port</span>
            <input
              type="number"
              value={config.web_port}
              onChange={(event) => {
                const parsed = Number(event.target.value);
                const clamped =
                  Number.isFinite(parsed) && parsed >= 1 && parsed <= 65535
                    ? Math.round(parsed)
                    : 0;
                setConfig({ ...config, web_port: clamped });
              }}
            />
          </label>
        </div>

        <div className="config-section-title">Providers</div>
        <div className="provider-list">
          {config.providers.map((provider) => (
            <div key={provider.id} className="provider-card">
              <div className="provider-card-header">
                <strong>{provider.label}</strong>
                <code className="provider-id">{provider.id}</code>
              </div>
              <div className="provider-card-body">
                <span className="provider-detail">
                  <span className="provider-detail-label">base_url</span>
                  <span className="provider-detail-value">
                    {provider.base_url}
                  </span>
                </span>
                <span className="provider-detail">
                  <span className="provider-detail-label">default</span>
                  <span className="provider-detail-value">
                    {provider.default_model}
                  </span>
                </span>
                {provider.models.length > 0 && (
                  <span className="provider-detail">
                    <span className="provider-detail-label">models</span>
                    <span className="provider-detail-value">
                      {provider.models.join(", ")}
                    </span>
                  </span>
                )}
                <span className="provider-detail">
                  <span className="provider-detail-label">api_key</span>
                  <span
                    className={`provider-detail-value ${provider.has_api_key ? "status-ok" : "status-none"}`}
                  >
                    {provider.has_api_key ? "configured" : "not set"}
                  </span>
                </span>
              </div>
            </div>
          ))}
        </div>

        <div className="config-section-title">Channel Overrides</div>
        {(["discord", "telegram"] as const).map((channel) => {
          const override = config.channel_overrides[channel] || {};
          return (
            <div key={channel} className="channel-override-row">
              <span className="channel-override-label">{channel}</span>
              <div className="grid-two">
                <label>
                  <span>Provider</span>
                  <select
                    value={override.provider || ""}
                    onChange={(event) => {
                      const value = event.target.value;
                      const provider = config.providers.find(
                        (item) => item.id === value,
                      );
                      setConfig({
                        ...config,
                        channel_overrides: {
                          ...config.channel_overrides,
                          [channel]: {
                            ...override,
                            provider: value || undefined,
                            model: provider?.default_model || undefined,
                          },
                        },
                      });
                    }}
                  >
                    <option value="">---</option>
                    {config.providers.map((p) => (
                      <option key={p.id} value={p.id}>
                        {p.label}
                      </option>
                    ))}
                  </select>
                </label>
                <label>
                  <span>Model</span>
                  <input
                    value={override.model || ""}
                    placeholder="Use default"
                    onChange={(event) => {
                      const value = event.target.value;
                      setConfig({
                        ...config,
                        channel_overrides: {
                          ...config.channel_overrides,
                          [channel]: {
                            ...override,
                            model: value || undefined,
                          },
                        },
                      });
                    }}
                  />
                </label>
              </div>
            </div>
          );
        })}

        <div className="config-footer">
          <span>Changes take effect on the next turn.</span>
          <button className="primary-button" type="submit">
            Save
          </button>
        </div>
      </form>
    </Modal>
  );
}
