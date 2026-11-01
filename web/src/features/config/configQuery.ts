import { fetchConfig, fetchConfigMcp, fetchConfigYaml } from "../../shared/api/config";
import { useServerState } from "../../shared/hooks/useServerState";

/** Prefix shared by every Config query, so one refresh reloads them all. */
export const CONFIG_QUERY_PREFIX = "config:";

export function configQueryKey(authToken: string): string {
  return `${CONFIG_QUERY_PREFIX}current:${authToken}`;
}

/**
 * The structured config. Polled so edits made in the YAML file or through
 * slash commands show up without a manual reload.
 */
export function useRuntimeConfig(authToken: string) {
  return useServerState(configQueryKey(authToken), () => fetchConfig(authToken), {
    pollIntervalMs: 10_000,
    revalidateOnMount: true,
  });
}

/** MCP definitions are files read on demand, so they are fetched only when shown. */
export function useConfigMcp(authToken: string) {
  return useServerState(`${CONFIG_QUERY_PREFIX}mcp:${authToken}`, () => fetchConfigMcp(authToken), {
    revalidateOnMount: true,
  });
}

export function useConfigYaml(authToken: string, revision: number | undefined) {
  return useServerState(`${CONFIG_QUERY_PREFIX}yaml:${revision ?? ""}:${authToken}`, () => fetchConfigYaml(authToken), {
    revalidateOnMount: true,
  });
}
