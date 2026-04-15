/** セッション一覧アイテム */
export type SessionItem = {
  session_key: string;
  label: string;
  chat_id: number;
  channel: string;
  last_message_time?: string;
  last_message_preview?: string | null;
};

/** メッセージアイテム */
export type MessageItem = {
  id: string;
  sender_name: string;
  content: string;
  is_from_bot: boolean;
  timestamp: string;
};

/** プロバイダー情報 */
export type ProviderInfo = {
  id: string;
  label: string;
  base_url: string;
  default_model: string;
  models: string[];
  has_api_key: boolean;
};

/** チャネル別オーバーライド */
export type ChannelOverride = {
  provider?: string;
  model?: string;
};

/** GET /api/config レスポンス内の設定ペイロード */
export type ConfigPayload = {
  default_provider: string;
  default_model: string;
  data_dir: string;
  workspace_dir: string;
  web_enabled: boolean;
  web_host: string;
  web_port: number;
  web_auth_enabled: boolean;
  has_api_key: boolean;
  config_path: string;
  providers: ProviderInfo[];
  channel_overrides: Record<string, ChannelOverride>;
};

/** プロバイダー更新用ペイロード */
export type ProviderUpdate = {
  label: string;
  base_url: string;
  default_model: string;
  models: string[];
  api_key?: string;
};

/** ヘルスチェックペイロード */
export type HealthPayload = {
  version?: string;
};

/** SSE ストリームイベント */
export type StreamEvent = {
  event: string;
  payload: Record<string, unknown>;
};

/** WebSocket リクエスト */
export type WsReq = {
  type: "req";
  id: string;
  method: string;
  params: Record<string, unknown>;
};

/** WebSocket レスポンス */
export type WsRes = {
  type: "res";
  id: string;
  ok: boolean;
  payload?: Record<string, unknown>;
  error?: { code?: string; message?: string };
};

/** WebSocket イベント */
export type WsEvent = {
  type: "event";
  event: string;
  payload?: Record<string, unknown>;
};

/** UI ステータス表示 */
export type UiStatus = {
  tone: "idle" | "ok" | "error";
  text: string;
};
