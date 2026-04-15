import { useEffect, useMemo, useState } from "react";

import { api, AuthRequiredError } from "../api";
import type { ConfigPayload, ProviderInfo, ProviderUpdate, UiStatus } from "../types";

type UseConfigArgs = {
  authTokenRef: React.MutableRefObject<string>;
  onAuthRequired: () => void;
  onStatusChange: (status: UiStatus) => void;
};

type UseConfigResult = {
  config: ConfigPayload | null;
  configApiKey: string;
  setConfigApiKey: React.Dispatch<React.SetStateAction<string>>;
  setConfig: React.Dispatch<React.SetStateAction<ConfigPayload | null>>;
  selectedProvider: ProviderInfo | null;
  refreshConfig: () => Promise<void>;
  saveConfig: () => Promise<void>;
};

export function useConfig({
  authTokenRef,
  onAuthRequired,
  onStatusChange,
}: UseConfigArgs): UseConfigResult {
  const [config, setConfig] = useState<ConfigPayload | null>(null);
  const [configApiKey, setConfigApiKey] = useState("");

  const selectedProvider = useMemo(() => {
    return (
      config?.providers.find((item) => item.id === config.default_provider) ??
      null
    );
  }, [config]);

  useEffect(() => {
    setConfigApiKey("");
    setConfig((current) =>
      current
        ? { ...current, has_api_key: selectedProvider?.has_api_key ?? false }
        : current,
    );
  }, [selectedProvider?.id]);

  async function refreshConfig() {
    const payload = await api<{ ok: boolean; config: ConfigPayload }>(
      "/api/config",
      authTokenRef.current,
    );
    setConfig(payload.config);
    setConfigApiKey("");
  }

  async function saveConfig() {
    if (!config) return;

    const providersPayload: Record<string, ProviderUpdate> = {};
    for (const provider of config.providers) {
      providersPayload[provider.id] = {
        label: provider.label,
        base_url: provider.base_url,
        default_model: provider.default_model,
        models: provider.models,
      };
    }

    const activeProviderId = config.default_provider;
    const activeProvider = providersPayload[activeProviderId];
    if (activeProvider) {
      const apiKey = configApiKey.trim();
      if (apiKey === "*CLEAR*") {
        activeProvider.api_key = apiKey;
      } else if (apiKey) {
        activeProvider.api_key = apiKey;
      }
    }

    const payload = {
      default_provider: config.default_provider,
      default_model: config.default_model,
      providers: providersPayload,
      web_enabled: config.web_enabled,
      web_host: config.web_host,
      web_port: config.web_port,
      channel_overrides: config.channel_overrides,
    };

    try {
      const response = await api<{ ok: boolean; config: ConfigPayload }>(
        "/api/config",
        authTokenRef.current,
        {
          method: "PUT",
          body: JSON.stringify(payload),
        },
      );
      setConfig(response.config);
      setConfigApiKey("");
      onStatusChange({
        tone: "ok",
        text: "Config saved. Changes take effect on the next turn.",
      });
    } catch (error) {
      if (error instanceof AuthRequiredError) {
        onAuthRequired();
      }
      onStatusChange({
        tone: "error",
        text: error instanceof Error ? error.message : "Failed to save config",
      });
      throw error;
    }
  }

  return {
    config,
    configApiKey,
    setConfigApiKey,
    setConfig,
    selectedProvider,
    refreshConfig,
    saveConfig,
  };
}
