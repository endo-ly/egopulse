// EgoPulse WebUI

let currentSession = 'main';

// DOM Elements
const sessionList = document.getElementById('session-list');
const messagesDiv = document.getElementById('messages');
const messageInput = document.getElementById('message-input');
const sendBtn = document.getElementById('send-btn');
const newSessionBtn = document.getElementById('new-session');
const currentSessionSpan = document.getElementById('current-session');
let activeAssistantMessage = null;

// Load sessions
async function loadSessions() {
    try {
        const response = await fetch('/api/sessions');
        const data = await response.json();

        sessionList.innerHTML = '';
        data.sessions.forEach(session => {
            const li = document.createElement('li');
            li.textContent = session.label || session.session_key;
            li.dataset.sessionKey = session.session_key;
            li.addEventListener('click', () => selectSession(session.session_key));
            if (session.session_key === currentSession) {
                li.classList.add('active');
            }
            sessionList.appendChild(li);
        });
    } catch (error) {
        console.error('Failed to load sessions:', error);
    }
}

// Select a session
async function selectSession(sessionKey) {
    currentSession = sessionKey;
    currentSessionSpan.textContent = sessionKey;

    // Update active state
    document.querySelectorAll('#session-list li').forEach(li => {
        li.classList.toggle('active', li.dataset.sessionKey === sessionKey);
    });

    // Load history
    await loadHistory(sessionKey);
}

// Load message history
async function loadHistory(sessionKey) {
    try {
        const response = await fetch(`/api/history?session_key=${encodeURIComponent(sessionKey)}`);
        const data = await response.json();

        messagesDiv.innerHTML = '';
        data.messages.forEach(msg => {
            addMessage(msg.sender_name, msg.content, msg.is_from_bot);
        });

        // Scroll to bottom
        messagesDiv.scrollTop = messagesDiv.scrollHeight;
    } catch (error) {
        console.error('Failed to load history:', error);
    }
}

// Add a message to the UI
function addMessage(sender, content, isBot) {
    const div = document.createElement('div');
    div.className = `message ${isBot ? 'bot' : 'user'}`;

    const senderDiv = document.createElement('div');
    senderDiv.className = 'sender';
    senderDiv.textContent = sender;

    const contentDiv = document.createElement('div');
    contentDiv.className = 'content';
    contentDiv.textContent = content;

    div.appendChild(senderDiv);
    div.appendChild(contentDiv);
    messagesDiv.appendChild(div);
    messagesDiv.scrollTop = messagesDiv.scrollHeight;
    return div;
}

function ensureAssistantMessage() {
    if (activeAssistantMessage && messagesDiv.contains(activeAssistantMessage)) {
        return activeAssistantMessage;
    }

    activeAssistantMessage = addMessage('Assistant', '', true);
    return activeAssistantMessage;
}

function appendAssistantDelta(delta) {
    if (!delta) {
        return;
    }

    const messageDiv = ensureAssistantMessage();
    const contentDiv = messageDiv.querySelector('.content');
    contentDiv.textContent += delta;
    messagesDiv.scrollTop = messagesDiv.scrollHeight;
}

function finalizeAssistantMessage(text) {
    if (activeAssistantMessage && messagesDiv.contains(activeAssistantMessage)) {
        const contentDiv = activeAssistantMessage.querySelector('.content');
        if (!contentDiv.textContent && text) {
            contentDiv.textContent = text;
        }
        activeAssistantMessage = null;
        messagesDiv.scrollTop = messagesDiv.scrollHeight;
        return;
    }

    if (text) {
        addMessage('Assistant', text, true);
    }
}

function parseJson(data) {
    try {
        return JSON.parse(data);
    } catch {
        return null;
    }
}

function handleSseEvent(eventName, data) {
    const payload = parseJson(data);

    switch (eventName) {
        case 'text_delta':
            appendAssistantDelta(payload?.delta || '');
            break;
        case 'final_response':
            finalizeAssistantMessage(payload?.text || '');
            break;
        case 'error':
            addMessage('System', payload?.message || data || 'Error: Failed to send message', true);
            activeAssistantMessage = null;
            break;
        default:
            break;
    }
}

function processSseBuffer(state, flush = false) {
    let newlineIndex = state.buffer.indexOf('\n');
    while (newlineIndex !== -1) {
        let line = state.buffer.slice(0, newlineIndex);
        state.buffer = state.buffer.slice(newlineIndex + 1);
        line = line.replace(/\r$/, '');

        if (line === '') {
            if (state.dataLines.length > 0) {
                handleSseEvent(state.eventName, state.dataLines.join('\n'));
                state.dataLines = [];
                state.eventName = 'message';
            }
        } else if (line.startsWith('event:')) {
            state.eventName = line.slice(6).trim();
        } else if (line.startsWith('data:')) {
            state.dataLines.push(line.slice(5).trimStart());
        }

        newlineIndex = state.buffer.indexOf('\n');
    }

    if (flush && state.buffer) {
        const line = state.buffer.replace(/\r$/, '');
        if (line.startsWith('event:')) {
            state.eventName = line.slice(6).trim();
        } else if (line.startsWith('data:')) {
            state.dataLines.push(line.slice(5).trimStart());
        }
        state.buffer = '';
    }

    if (flush && state.dataLines.length > 0) {
        handleSseEvent(state.eventName, state.dataLines.join('\n'));
        state.dataLines = [];
        state.eventName = 'message';
    }
}

// Send a message
async function sendMessage() {
    const message = messageInput.value.trim();
    if (!message) return;

    messageInput.value = '';

    // Add user message to UI
    addMessage('You', message, false);
    activeAssistantMessage = null;

    try {
        const response = await fetch('/api/send_stream', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({
                session_key: currentSession,
                sender_name: 'web-user',
                message: message
            })
        });

        if (!response.ok) {
            const errorText = (await response.text()).trim();
            addMessage('System', errorText || `Error: Server returned ${response.status}`, true);
            return;
        }

        if (!response.body) {
            addMessage('System', 'Error: Empty response body', true);
            return;
        }

        // Read SSE stream
        const reader = response.body.getReader();
        const decoder = new TextDecoder();
        const sseState = {
            buffer: '',
            eventName: 'message',
            dataLines: []
        };

        while (true) {
            const { done, value } = await reader.read();
            if (done) {
                break;
            }

            sseState.buffer += decoder.decode(value, { stream: true });
            processSseBuffer(sseState);
        }

        sseState.buffer += decoder.decode();
        processSseBuffer(sseState, true);
    } catch (error) {
        console.error('Failed to send message:', error);
        addMessage('System', 'Error: Failed to send message', true);
        activeAssistantMessage = null;
    }
}

// Create new session
newSessionBtn.addEventListener('click', () => {
    const sessionKey = 'session-' + Date.now();
    selectSession(sessionKey);
});

// Send on Enter
messageInput.addEventListener('keypress', (e) => {
    if (e.key === 'Enter') {
        sendMessage();
    }
});

// Send button click
sendBtn.addEventListener('click', sendMessage);

// Initial load
loadSessions();
selectSession('main');
