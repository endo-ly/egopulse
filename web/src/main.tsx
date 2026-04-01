import React, { FormEvent, useEffect, useMemo, useRef, useState } from 'react'
import { createRoot } from 'react-dom/client'
import './styles.css'

type SessionItem = {
  session_key: string
  label: string
  chat_id: number
  channel: string
  last_message_time?: string
  last_message_preview?: string | null
}

type MessageItem = {
  id: string
  sender_name: string
  content: string
  is_from_bot: boolean
  timestamp: string
}

type ConfigPayload = {
  model: string
  base_url: string
  data_dir: string
  web_enabled: boolean
  web_host: string
  web_port: number
  has_api_key: boolean
  config_path: string
  requires_restart: boolean
}

type HealthPayload = {
  version?: string
}

type GatewayEventPayload = {
  runId: string
  sessionKey: string
  seq: number
  state: 'delta' | 'done' | 'error'
  message?: {
    role: 'assistant'
    content: Array<{ type: 'text'; text: string }>
  }
  errorMessage?: string
}

type WsReq = {
  type: 'req'
  id: string
  method: string
  params: Record<string, unknown>
}

type WsRes = {
  type: 'res'
  id: string
  ok: boolean
  payload?: Record<string, unknown>
  error?: { code?: string; message?: string }
}

type WsEvent = {
  type: 'event'
  event: string
  payload?: Record<string, unknown>
}

type UiStatus = {
  tone: 'idle' | 'ok' | 'error'
  text: string
}

const defaultStatus: UiStatus = { tone: 'idle', text: 'Ready' }

function api<T>(path: string, options?: RequestInit): Promise<T> {
  return fetch(path, {
    ...options,
    headers: {
      'Content-Type': 'application/json',
      ...(options?.headers || {}),
    },
  }).then(async (res) => {
    const data = await res.json().catch(() => ({}))
    if (!res.ok) {
      throw new Error(String(data.error || data.message || `HTTP ${res.status}`))
    }
    return data as T
  })
}

function wsUrl(): string {
  const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
  return `${protocol}//${window.location.host}/ws`
}

function nowIso(): string {
  return new Date().toISOString()
}

function sessionKeyNow(): string {
  const d = new Date()
  const pad = (v: number) => String(v).padStart(2, '0')
  return `session-${d.getFullYear()}${pad(d.getMonth() + 1)}${pad(d.getDate())}${pad(d.getHours())}${pad(d.getMinutes())}${pad(d.getSeconds())}`
}

function buildDraftId(runId: string): string {
  return `draft:${runId}`
}

function makeId(prefix: string): string {
  const cryptoApi = globalThis.crypto as Crypto | undefined
  if (cryptoApi && typeof cryptoApi.randomUUID === 'function') {
    return `${prefix}:${cryptoApi.randomUUID()}`
  }

  if (cryptoApi && typeof cryptoApi.getRandomValues === 'function') {
    const bytes = new Uint8Array(16)
    cryptoApi.getRandomValues(bytes)
    const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('')
    return `${prefix}:${hex}`
  }

  return `${prefix}:${Date.now().toString(36)}:${Math.random().toString(36).slice(2)}`
}

function App() {
  const [sessions, setSessions] = useState<SessionItem[]>([])
  const [selectedSession, setSelectedSession] = useState<string>('')
  const [messages, setMessages] = useState<MessageItem[]>([])
  const [draft, setDraft] = useState('')
  const [config, setConfig] = useState<ConfigPayload | null>(null)
  const [configApiKey, setConfigApiKey] = useState('')
  const [showSettings, setShowSettings] = useState(false)
  const [health, setHealth] = useState<HealthPayload>({})
  const [status, setStatus] = useState<UiStatus>(defaultStatus)
  const [wsState, setWsState] = useState<'connecting' | 'open' | 'closed'>('connecting')
  const socketRef = useRef<WebSocket | null>(null)
  const connectResolver = useRef<(() => void) | null>(null)
  const connectPromise = useRef<Promise<void> | null>(null)
  const requestResolvers = useRef(new Map<string, { resolve: (value: WsRes) => void; reject: (error: Error) => void }>())
  const messageEndRef = useRef<HTMLDivElement | null>(null)

  const selectedLabel = useMemo(() => {
    return sessions.find((item) => item.session_key === selectedSession)?.label || selectedSession
  }, [selectedSession, sessions])

  useEffect(() => {
    messageEndRef.current?.scrollIntoView({ block: 'end' })
  }, [messages])

  useEffect(() => {
    void refreshHealth()
    void refreshConfig()
    void refreshSessions()
    void connectGateway()
    return () => {
      socketRef.current?.close()
    }
  }, [])

  async function refreshHealth() {
    const payload = await api<{ ok: boolean; version: string }>('/api/health')
    setHealth({ version: payload.version })
  }

  async function refreshConfig() {
    const payload = await api<{ ok: boolean; config: ConfigPayload }>('/api/config')
    setConfig(payload.config)
    setConfigApiKey('')
  }

  async function refreshSessions(preferredKey?: string) {
    const payload = await api<{ ok: boolean; sessions: SessionItem[] }>('/api/sessions')
    setSessions(payload.sessions)

    const nextKey = preferredKey || selectedSession || payload.sessions[0]?.session_key || sessionKeyNow()
    setSelectedSession(nextKey)
    await loadHistory(nextKey)
  }

  async function loadHistory(sessionKey: string) {
    const payload = await api<{ ok: boolean; messages: MessageItem[] }>(`/api/history?session_key=${encodeURIComponent(sessionKey)}`)
    setMessages(payload.messages)
  }

  async function connectGateway() {
    if (socketRef.current && socketRef.current.readyState === WebSocket.OPEN) {
      return
    }
    if (connectPromise.current) {
      return connectPromise.current
    }

    setWsState('connecting')
    connectPromise.current = new Promise<void>((resolve, reject) => {
      connectResolver.current = resolve
      const socket = new WebSocket(wsUrl())
      socketRef.current = socket

      socket.addEventListener('message', (event) => {
        const data = JSON.parse(String(event.data)) as WsRes | WsEvent
        if (data.type === 'event') {
          if (data.event === 'connect.challenge') {
            const connectReq: WsReq = {
              type: 'req',
              id: 'connect',
              method: 'connect',
              params: { minProtocol: 1, maxProtocol: 1 },
            }
            socket.send(JSON.stringify(connectReq))
            return
          }

          if (data.event === 'chat') {
            applyGatewayEvent(data.payload as unknown as GatewayEventPayload)
          }
          return
        }

        const resolver = requestResolvers.current.get(data.id)
        if (resolver) {
          requestResolvers.current.delete(data.id)
          resolver.resolve(data)
        }

        if (data.id === 'connect' && data.ok) {
          setWsState('open')
          setStatus({ tone: 'ok', text: 'Gateway connected' })
          connectResolver.current?.()
          connectResolver.current = null
          connectPromise.current = null
        }
      })

      socket.addEventListener('close', () => {
        setWsState('closed')
        connectPromise.current = null
        socketRef.current = null
      })

      socket.addEventListener('error', () => {
        setWsState('closed')
        setStatus({ tone: 'error', text: 'Gateway connection failed' })
        connectPromise.current = null
        reject(new Error('websocket error'))
      })
    })

    return connectPromise.current
  }

  function sendGatewayRequest(method: string, params: Record<string, unknown>): Promise<WsRes> {
    const socket = socketRef.current
    if (!socket || socket.readyState !== WebSocket.OPEN) {
      return Promise.reject(new Error('gateway is not connected'))
    }
    const id = makeId(method)
    const request: WsReq = { type: 'req', id, method, params }
    return new Promise<WsRes>((resolve, reject) => {
      requestResolvers.current.set(id, { resolve, reject })
      socket.send(JSON.stringify(request))
      window.setTimeout(() => {
        if (requestResolvers.current.has(id)) {
          requestResolvers.current.delete(id)
          reject(new Error(`${method} timed out`))
        }
      }, 30000)
    })
  }

  function applyGatewayEvent(payload: GatewayEventPayload) {
    if (!payload || payload.sessionKey !== selectedSession) {
      return
    }

    const text = payload.message?.content?.map((item) => item.text).join('') || ''
    const draftId = buildDraftId(payload.runId)

    if (payload.state === 'delta') {
      setMessages((prev) => {
        const existing = prev.find((item) => item.id === draftId)
        if (existing) {
          return prev.map((item) => item.id === draftId ? { ...item, content: item.content + text } : item)
        }
        return [...prev, { id: draftId, sender_name: 'egopulse', content: text, is_from_bot: true, timestamp: nowIso() }]
      })
      return
    }

    if (payload.state === 'done') {
      setMessages((prev) => prev.map((item) => item.id === draftId ? { ...item, id: `${draftId}:done` } : item))
      setStatus({ tone: 'ok', text: 'Response received' })
      return
    }

    if (payload.state === 'error') {
      setStatus({ tone: 'error', text: payload.errorMessage || 'Gateway run failed' })
      setMessages((prev) => prev.filter((item) => item.id !== draftId))
    }
  }

  async function handleNewSession() {
    const key = sessionKeyNow()
    setSelectedSession(key)
    setMessages([])
    setSessions((prev) => [{ session_key: key, label: key, chat_id: 0, channel: 'web', last_message_time: nowIso(), last_message_preview: null }, ...prev])
  }

  async function handleSend(event: FormEvent) {
    event.preventDefault()
    const text = draft.trim()
    if (!text) return

    const sessionKey = selectedSession || sessionKeyNow()
    if (!selectedSession) {
      setSelectedSession(sessionKey)
    }

    setMessages((prev) => [...prev, {
      id: makeId('message'),
      sender_name: 'web-user',
      content: text,
      is_from_bot: false,
      timestamp: nowIso(),
    }])
    setDraft('')
    setStatus({ tone: 'idle', text: 'Waiting for response…' })

    try {
      await connectGateway()
      const response = await sendGatewayRequest('chat.send', {
        sessionKey,
        message: text,
      })
      if (!response.ok) {
        throw new Error(response.error?.message || 'chat.send failed')
      }
      await refreshSessions(sessionKey)
    } catch (error) {
      setStatus({ tone: 'error', text: error instanceof Error ? error.message : 'Failed to send message' })
    }
  }

  async function handleSaveConfig(event: FormEvent) {
    event.preventDefault()
    if (!config) return

    const payload = {
      model: config.model,
      base_url: config.base_url,
      data_dir: config.data_dir,
      web_enabled: config.web_enabled,
      web_host: config.web_host,
      web_port: config.web_port,
      api_key: configApiKey,
    }

    try {
      const response = await api<{ ok: boolean; config: ConfigPayload; requires_restart: boolean }>('/api/config', {
        method: 'PUT',
        body: JSON.stringify(payload),
      })
      setConfig(response.config)
      setConfigApiKey('')
      setStatus({ tone: 'ok', text: response.requires_restart ? 'Config saved. Restart required for runtime changes.' : 'Config saved.' })
      setShowSettings(false)
    } catch (error) {
      setStatus({ tone: 'error', text: error instanceof Error ? error.message : 'Failed to save config' })
    }
  }

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand">
          <img src="/icon.png" alt="EgoPulse" />
          <div>
            <h1>EgoPulse</h1>
            <p>{health.version ? `v${health.version}` : 'Web'}</p>
          </div>
        </div>

        <button className="primary-button" onClick={() => void handleNewSession()}>New Session</button>
        <button className="secondary-button" onClick={() => setShowSettings(true)}>Runtime Config</button>

        <div className="sidebar-section">
          <div className="sidebar-title-row">
            <h2>Sessions</h2>
            <span>{sessions.length}</span>
          </div>
          <div className="session-list">
            {sessions.map((item) => (
              <button
                key={item.session_key}
                className={item.session_key === selectedSession ? 'session-item active' : 'session-item'}
                onClick={() => {
                  setSelectedSession(item.session_key)
                  void loadHistory(item.session_key)
                }}
              >
                <strong>{item.label}</strong>
                <small>{item.last_message_preview || 'No messages yet'}</small>
              </button>
            ))}
          </div>
        </div>
      </aside>

      <main className="main-panel">
        <header className="chat-header">
          <div>
            <h2>{selectedLabel || 'Select a session'}</h2>
            <p>Gateway: {wsState}</p>
          </div>
          <div className={`status-badge ${status.tone}`}>{status.text}</div>
        </header>

        <section className="timeline">
          {messages.map((message) => (
            <article key={message.id} className={message.is_from_bot ? 'bubble bubble-bot' : 'bubble bubble-user'}>
              <div className="bubble-meta">
                <span>{message.sender_name}</span>
                <time>{new Date(message.timestamp).toLocaleTimeString()}</time>
              </div>
              <pre>{message.content}</pre>
            </article>
          ))}
          <div ref={messageEndRef} />
        </section>

        <form className="composer" onSubmit={handleSend}>
          <textarea
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            placeholder="Type a message"
            rows={3}
          />
          <button className="primary-button" type="submit">Send</button>
        </form>
      </main>

      {showSettings && config ? (
        <div className="modal-backdrop" onClick={() => setShowSettings(false)}>
          <div className="modal-card" onClick={(event) => event.stopPropagation()}>
            <div className="modal-header">
              <div>
                <h3>Runtime Config</h3>
                <p>{config.config_path}</p>
              </div>
              <button className="icon-button" onClick={() => setShowSettings(false)}>×</button>
            </div>

            <form className="config-form" onSubmit={handleSaveConfig}>
              <label>
                <span>Model</span>
                <input value={config.model} onChange={(event) => setConfig({ ...config, model: event.target.value })} />
              </label>
              <label>
                <span>Base URL</span>
                <input value={config.base_url} onChange={(event) => setConfig({ ...config, base_url: event.target.value })} />
              </label>
              <label>
                <span>API Key</span>
                <input type="password" value={configApiKey} placeholder={config.has_api_key ? 'Configured. Enter to replace.' : 'Enter API key'} onChange={(event) => setConfigApiKey(event.target.value)} />
              </label>
              <label>
                <span>Data Dir</span>
                <input value={config.data_dir} onChange={(event) => setConfig({ ...config, data_dir: event.target.value })} />
              </label>
              <div className="grid-two">
                <label>
                  <span>Web Host</span>
                  <input value={config.web_host} onChange={(event) => setConfig({ ...config, web_host: event.target.value })} />
                </label>
                <label>
                  <span>Web Port</span>
                  <input type="number" value={config.web_port} onChange={(event) => setConfig({ ...config, web_port: Number(event.target.value) || 0 })} />
                </label>
              </div>
              <label className="checkbox-row">
                <input type="checkbox" checked={config.web_enabled} onChange={(event) => setConfig({ ...config, web_enabled: event.target.checked })} />
                <span>Enable web channel</span>
              </label>
              <div className="config-footer">
                <span>{config.requires_restart ? 'Changes are persisted to disk. Restart EgoPulse to apply runtime changes.' : ''}</span>
                <button className="primary-button" type="submit">Save</button>
              </div>
            </form>
          </div>
        </div>
      ) : null}
    </div>
  )
}

createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
)
