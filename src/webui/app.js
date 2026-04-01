// EgoPulse WebUI

let currentSession = 'main';

// DOM Elements
const sessionList = document.getElementById('session-list');
const messagesDiv = document.getElementById('messages');
const messageInput = document.getElementById('message-input');
const sendBtn = document.getElementById('send-btn');
const newSessionBtn = document.getElementById('new-session');
const currentSessionSpan = document.getElementById('current-session');

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
    div.innerHTML = `
        <div class="sender">${sender}</div>
        <div class="content">${escapeHtml(content)}</div>
    `;
    messagesDiv.appendChild(div);
    messagesDiv.scrollTop = messagesDiv.scrollHeight;
}

// Escape HTML
function escapeHtml(text) {
    const div = document.createElement('div');
    div.textContent = text;
    return div.innerHTML;
}

// Send a message
async function sendMessage() {
    const message = messageInput.value.trim();
    if (!message) return;

    messageInput.value = '';

    // Add user message to UI
    addMessage('You', message, false);

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

        // Read SSE stream
        const reader = response.body.getReader();
        const decoder = new TextDecoder();
        let buffer = '';

        while (true) {
            const { done, value } = await reader.read();
            if (done) break;

            buffer += decoder.decode(value, { stream: true });
            const lines = buffer.split('\n');
            buffer = lines.pop() || '';

            for (const line of lines) {
                if (line.startsWith('data:')) {
                    const data = line.slice(5);
                    if (data) {
                        addMessage('Assistant', data, true);
                    }
                }
            }
        }
    } catch (error) {
        console.error('Failed to send message:', error);
        addMessage('System', 'Error: Failed to send message', true);
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
