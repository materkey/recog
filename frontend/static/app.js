/**
 * Recog Transcripts - Vanilla JS SPA
 */

// --- API ---

const api = {
    async getMeetings() {
        const res = await fetch('/api/meetings');
        if (!res.ok) throw new Error('Failed to fetch meetings');
        return res.json();
    },

    async getMeeting(id) {
        const res = await fetch(`/api/meetings/${id}`);
        if (!res.ok) throw new Error('Meeting not found');
        return res.json();
    },

    async getTranscript(id) {
        const res = await fetch(`/api/meetings/${id}/transcript`);
        if (!res.ok) throw new Error('Transcript not found');
        return res.json();
    },

    async getLiveStatus() {
        const res = await fetch('/api/live/status');
        if (!res.ok) throw new Error('Failed to get live status');
        return res.json();
    },

    async searchTranscripts(query) {
        const res = await fetch(`/api/search?q=${encodeURIComponent(query)}`);
        if (!res.ok) throw new Error('Search failed');
        return res.json();
    }
};


// --- Router ---

const router = {
    routes: {
        '/': 'meetings',
        '/live': 'live',
        '/meeting/:id': 'meeting'
    },

    init() {
        window.addEventListener('hashchange', () => this.handleRoute());
        this.handleRoute();
    },

    navigate(path) {
        window.location.hash = path;
    },

    handleRoute() {
        const hash = window.location.hash.slice(1) || '/';

        // Update nav links
        document.querySelectorAll('.nav-link').forEach(link => {
            link.classList.remove('active');
            if (link.getAttribute('href') === '#' + hash) {
                link.classList.add('active');
            }
        });

        // Match route
        if (hash === '/' || hash === '') {
            this.showView('meetings');
            views.meetings.load();
        } else if (hash === '/live') {
            this.showView('live');
            views.live.connect();
        } else if (hash.startsWith('/meeting/')) {
            const id = hash.replace('/meeting/', '');
            this.showView('meeting');
            views.meeting.load(id);
        }
    },

    showView(name) {
        // Disconnect live if leaving
        if (name !== 'live') {
            views.live.disconnect();
        }

        document.querySelectorAll('.view').forEach(v => v.style.display = 'none');
        const view = document.getElementById('view-' + name);
        if (view) view.style.display = 'block';
    }
};


// --- Views ---

const views = {
    meetings: {
        meetings: [],
        filterSource: localStorage.getItem('filterSource') ?? 'ktalk',
        filterTitle: localStorage.getItem('filterTitle') ?? '',
        searchMode: localStorage.getItem('searchMode') ?? 'title',
        searchTimeout: null,

        async load() {
            const container = document.getElementById('meetings-list');
            container.innerHTML = '<div class="loading">Loading meetings...</div>';

            // Restore filter values to UI
            const sourceSelect = document.getElementById('filter-source');
            const titleInput = document.getElementById('filter-title');
            const searchModeSelect = document.getElementById('search-mode');
            sourceSelect.value = this.filterSource;
            titleInput.value = this.filterTitle;
            searchModeSelect.value = this.searchMode;

            // Init date picker
            DatePicker.init('filter-date', () => this.render());

            try {
                this.meetings = await api.getMeetings();
                this.render();
            } catch (e) {
                container.innerHTML = `<div class="error">Error: ${e.message}</div>`;
            }

            // Setup source filter
            sourceSelect.addEventListener('change', (e) => {
                this.filterSource = e.target.value;
                localStorage.setItem('filterSource', this.filterSource);
                this.render();
            });

            // Setup search mode
            searchModeSelect.addEventListener('change', (e) => {
                this.searchMode = e.target.value;
                localStorage.setItem('searchMode', this.searchMode);
                this.handleSearch();
            });

            // Setup title/content filter with debounce
            titleInput.addEventListener('input', (e) => {
                this.filterTitle = e.target.value;
                localStorage.setItem('filterTitle', this.filterTitle);

                clearTimeout(this.searchTimeout);
                this.searchTimeout = setTimeout(() => this.handleSearch(), 300);
            });
        },

        handleSearch() {
            if (this.searchMode === 'content' && this.filterTitle.length >= 2) {
                this.searchContent();
            } else {
                document.getElementById('search-results').style.display = 'none';
                document.getElementById('meetings-list').style.display = '';
                this.render();
            }
        },

        async searchContent() {
            const container = document.getElementById('meetings-list');
            const resultsContainer = document.getElementById('search-results');

            container.style.display = 'none';
            resultsContainer.style.display = '';
            resultsContainer.innerHTML = '<div class="loading">Searching...</div>';

            try {
                const results = await api.searchTranscripts(this.filterTitle);
                this.renderSearchResults(results);
            } catch (e) {
                resultsContainer.innerHTML = `<div class="error">Error: ${e.message}</div>`;
            }
        },

        renderSearchResults(results) {
            const container = document.getElementById('search-results');

            if (results.length === 0) {
                container.innerHTML = '<div class="empty">No matches found</div>';
                return;
            }

            container.innerHTML = results.map(r => `
                <div class="search-result">
                    <a href="#/meeting/${r.meeting_id}" class="search-result-header">
                        <span class="search-result-title">${escapeHtml(r.title)}</span>
                        <span class="search-result-date">${formatDate(r.created_at)}</span>
                    </a>
                    <div class="search-result-matches">
                        ${r.matches.map(m => `
                            <div class="search-match">
                                <span class="match-time">${m.ts_display}</span>
                                <span class="match-speaker">${escapeHtml(m.speaker)}:</span>
                                <span class="match-text">${this.highlightSearch(escapeHtml(m.text), this.filterTitle)}</span>
                            </div>
                        `).join('')}
                    </div>
                </div>
            `).join('');
        },

        render() {
            const container = document.getElementById('meetings-list');
            let filtered = this.meetings;

            // Filter by date
            filtered = DatePicker.filterMeetings(filtered, DatePicker.getDate());

            // Filter by source
            if (this.filterSource) {
                if (this.filterSource === 'active') {
                    filtered = filtered.filter(m => m.is_active);
                } else {
                    filtered = filtered.filter(m => m.source === this.filterSource);
                }
            }

            // Filter by title
            if (this.filterTitle) {
                const term = this.filterTitle.toLowerCase();
                filtered = filtered.filter(m =>
                    (m.title || '').toLowerCase().includes(term)
                );
            }

            if (filtered.length === 0) {
                container.innerHTML = '<div class="empty">No meetings found</div>';
                return;
            }

            container.innerHTML = filtered.map(m => this.renderMeetingCard(m)).join('');
        },

        renderMeetingCard(meeting) {
            const date = new Date(meeting.created_at);
            const dateStr = date.toLocaleDateString('ru-RU', {
                day: 'numeric',
                month: 'short',
                year: 'numeric'
            });
            const timeStr = date.toLocaleTimeString('ru-RU', {
                hour: '2-digit',
                minute: '2-digit'
            });

            const duration = meeting.duration_seconds
                ? formatDuration(meeting.duration_seconds)
                : '-';

            const participants = (meeting.participants || []).slice(0, 3).join(', ');
            const moreParticipants = (meeting.participants || []).length > 3
                ? ` +${meeting.participants.length - 3}`
                : '';

            const sourceClass = `source-${meeting.source || 'unknown'}`;
            const sourceLabel = meeting.source || 'unknown';
            const isActive = meeting.is_active;
            const cardClass = isActive ? 'meeting-card meeting-card-active' : 'meeting-card';

            return `
                <a href="#/meeting/${meeting.id}" class="${cardClass}">
                    <div class="meeting-card-header">
                        <div class="meeting-card-badges">
                            ${isActive ? '<span class="badge-live">LIVE</span>' : ''}
                            <span class="meeting-source ${sourceClass}">${sourceLabel}</span>
                        </div>
                        <span class="meeting-date">${dateStr} ${timeStr}</span>
                    </div>
                    <h3 class="meeting-card-title">${escapeHtml(meeting.title || 'Untitled')}</h3>
                    <div class="meeting-card-meta">
                        <span class="meta-item">
                            <span class="meta-icon">&#9201;</span>
                            ${duration}
                        </span>
                        <span class="meta-item">
                            <span class="meta-icon">&#128172;</span>
                            ${meeting.segment_count || 0} segments
                        </span>
                    </div>
                    ${participants ? `
                        <div class="meeting-card-participants">
                            ${escapeHtml(participants)}${moreParticipants}
                        </div>
                    ` : ''}
                </a>
            `;
        }
    },

    meeting: {
        id: null,
        meta: null,
        segments: [],

        async load(id) {
            this.id = id;
            const titleEl = document.getElementById('meeting-title');
            const metaEl = document.getElementById('meeting-meta');
            const transcriptEl = document.getElementById('transcript');

            titleEl.textContent = 'Loading...';
            metaEl.innerHTML = '';
            transcriptEl.innerHTML = '<div class="loading">Loading transcript...</div>';

            try {
                const [meta, segments] = await Promise.all([
                    api.getMeeting(id),
                    api.getTranscript(id)
                ]);

                this.meta = meta;
                this.segments = segments;

                titleEl.textContent = meta.title || 'Untitled Meeting';
                this.renderMeta(meta, metaEl);
                this.renderTranscript(segments, transcriptEl);

                // Setup search
                document.getElementById('transcript-search').addEventListener('input', (e) => {
                    this.filterTranscript(e.target.value);
                });
            } catch (e) {
                transcriptEl.innerHTML = `<div class="error">Error: ${e.message}</div>`;
            }
        },

        renderMeta(meta, container) {
            const date = new Date(meta.created_at);
            const dateStr = date.toLocaleDateString('ru-RU', {
                weekday: 'long',
                day: 'numeric',
                month: 'long',
                year: 'numeric'
            });
            const timeStr = date.toLocaleTimeString('ru-RU', {
                hour: '2-digit',
                minute: '2-digit'
            });

            const duration = meta.stats?.duration_seconds
                ? formatDuration(meta.stats.duration_seconds)
                : '-';

            const participants = meta.participants
                ? Object.values(meta.participants).map(p => p.name).join(', ')
                : '-';

            container.innerHTML = `
                <div class="meta-grid">
                    <div class="meta-block">
                        <span class="meta-label">Date</span>
                        <span class="meta-value">${dateStr}</span>
                    </div>
                    <div class="meta-block">
                        <span class="meta-label">Time</span>
                        <span class="meta-value">${timeStr}</span>
                    </div>
                    <div class="meta-block">
                        <span class="meta-label">Duration</span>
                        <span class="meta-value">${duration}</span>
                    </div>
                    <div class="meta-block">
                        <span class="meta-label">Source</span>
                        <span class="meta-value source-${meta.source || 'unknown'}">${meta.source || 'unknown'}</span>
                    </div>
                    <div class="meta-block">
                        <span class="meta-label">Segments</span>
                        <span class="meta-value">${meta.stats?.segment_count || 0}</span>
                    </div>
                    <div class="meta-block">
                        <span class="meta-label">Words</span>
                        <span class="meta-value">${meta.stats?.words_count || 0}</span>
                    </div>
                </div>
                ${participants !== '-' ? `
                    <div class="meta-participants">
                        <span class="meta-label">Participants:</span>
                        <span class="meta-value">${escapeHtml(participants)}</span>
                    </div>
                ` : ''}
            `;
        },

        renderTranscript(segments, container, searchTerm = '') {
            if (segments.length === 0) {
                container.innerHTML = '<div class="empty">No transcript segments</div>';
                return;
            }

            // Group by speaker
            const grouped = this.groupSegments(segments);

            container.innerHTML = grouped.map(group => {
                // Системные события отображаются как разделители
                if (group.event_type) {
                    return `
                        <div class="transcript-event ${group.event_type}">
                            <span class="event-time">${group.ts_display}</span>
                            <span class="event-text">${escapeHtml(group.text)}</span>
                        </div>
                    `;
                }

                const speakerClass = group.is_local ? 'speaker-local' : 'speaker-other';
                const text = searchTerm
                    ? this.highlightSearch(group.text, searchTerm)
                    : escapeHtml(group.text);

                return `
                    <div class="transcript-segment ${speakerClass}">
                        <div class="segment-header">
                            <span class="segment-speaker">${escapeHtml(group.speaker)}</span>
                            <span class="segment-time">${group.ts_display}</span>
                        </div>
                        <div class="segment-text">${text}</div>
                    </div>
                `;
            }).join('');
        },

        groupSegments(segments) {
            const groups = [];
            let current = null;

            for (const seg of segments) {
                // Системные события не группируются
                if (seg.event_type) {
                    if (current) groups.push(current);
                    current = null;
                    groups.push({
                        speaker: seg.speaker,
                        speaker_id: seg.speaker_id,
                        is_local: false,
                        ts_display: seg.ts_display,
                        text: seg.text,
                        event_type: seg.event_type
                    });
                    continue;
                }

                if (current && current.speaker_id === seg.speaker_id && seg.cont) {
                    current.text += ' ' + seg.text;
                } else {
                    if (current) groups.push(current);
                    current = {
                        speaker: seg.speaker,
                        speaker_id: seg.speaker_id,
                        is_local: seg.is_local,
                        ts_display: seg.ts_display,
                        text: seg.text
                    };
                }
            }
            if (current) groups.push(current);

            return groups;
        },

        filterTranscript(searchTerm) {
            const container = document.getElementById('transcript');

            if (!searchTerm) {
                this.renderTranscript(this.segments, container);
                return;
            }

            const term = searchTerm.toLowerCase();
            const filtered = this.segments.filter(s =>
                s.text.toLowerCase().includes(term) ||
                s.speaker.toLowerCase().includes(term)
            );

            this.renderTranscript(filtered, container, searchTerm);
        },

        highlightSearch(text, term) {
            const escaped = escapeHtml(text);
            const regex = new RegExp(`(${escapeRegex(term)})`, 'gi');
            return escaped.replace(regex, '<mark>$1</mark>');
        }
    },

    live: {
        ws: null,
        segments: [],
        currentMeeting: null,

        connect() {
            const statusEl = document.getElementById('live-status');
            const infoEl = document.getElementById('live-meeting-info');
            const transcriptEl = document.getElementById('live-transcript');

            this.updateStatus('connecting');
            transcriptEl.innerHTML = '';

            const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
            this.ws = new WebSocket(`${protocol}//${window.location.host}/api/live/stream`);

            this.ws.onopen = () => {
                this.updateStatus('connected');
            };

            this.ws.onclose = () => {
                this.updateStatus('disconnected');
            };

            this.ws.onerror = () => {
                this.updateStatus('error');
            };

            this.ws.onmessage = (event) => {
                const msg = JSON.parse(event.data);
                this.handleMessage(msg);
            };
        },

        disconnect() {
            if (this.ws) {
                this.ws.close();
                this.ws = null;
            }
            this.segments = [];
            this.currentMeeting = null;
        },

        updateStatus(status) {
            const statusEl = document.getElementById('live-status');
            const dot = statusEl.querySelector('.status-dot');
            const text = statusEl.querySelector('.status-text');

            dot.className = 'status-dot status-' + status;

            const labels = {
                connecting: 'Connecting...',
                connected: 'Connected',
                disconnected: 'Disconnected',
                error: 'Connection error'
            };
            text.textContent = labels[status] || status;
        },

        handleMessage(msg) {
            const infoEl = document.getElementById('live-meeting-info');
            const transcriptEl = document.getElementById('live-transcript');

            switch (msg.type) {
                case 'init':
                    this.currentMeeting = msg.meeting;
                    this.segments = msg.segments || [];
                    this.renderMeetingInfo(msg.meeting, infoEl);
                    this.renderTranscript(transcriptEl);
                    break;

                case 'meeting_change':
                    this.currentMeeting = msg.meeting;
                    this.segments = [];
                    this.renderMeetingInfo(msg.meeting, infoEl);
                    this.renderTranscript(transcriptEl);
                    break;

                case 'segments':
                    this.segments.push(...msg.data);
                    this.appendSegments(msg.data, transcriptEl);
                    break;

                case 'error':
                    infoEl.innerHTML = `<div class="error">${msg.message}</div>`;
                    break;
            }
        },

        renderMeetingInfo(meeting, container) {
            if (!meeting) {
                container.innerHTML = '<div class="empty">No active meeting</div>';
                return;
            }

            const date = new Date(meeting.created_at);
            const dateStr = date.toLocaleDateString('ru-RU');
            const timeStr = date.toLocaleTimeString('ru-RU', {
                hour: '2-digit',
                minute: '2-digit'
            });

            container.innerHTML = `
                <div class="live-meeting-card">
                    <span class="meeting-source source-${meeting.source || 'unknown'}">${meeting.source || 'unknown'}</span>
                    <span class="meeting-title">${escapeHtml(meeting.title || 'Untitled')}</span>
                    <span class="meeting-date">${dateStr} ${timeStr}</span>
                </div>
            `;
        },

        renderTranscript(container) {
            if (this.segments.length === 0) {
                container.innerHTML = '<div class="empty live-waiting">Waiting for transcription...</div>';
                return;
            }

            container.innerHTML = '';
            this.appendSegments(this.segments, container);
        },

        appendSegments(segments, container) {
            // Remove waiting message if present
            const waiting = container.querySelector('.live-waiting');
            if (waiting) waiting.remove();

            for (const seg of segments) {
                const speakerClass = seg.is_local ? 'speaker-local' : 'speaker-other';

                const div = document.createElement('div');
                div.className = `transcript-segment ${speakerClass} segment-new`;
                div.innerHTML = `
                    <div class="segment-header">
                        <span class="segment-speaker">${escapeHtml(seg.speaker)}</span>
                        <span class="segment-time">${seg.ts_display}</span>
                    </div>
                    <div class="segment-text">${escapeHtml(seg.text)}</div>
                `;

                container.appendChild(div);

                // Remove animation class after animation
                setTimeout(() => div.classList.remove('segment-new'), 500);
            }

            // Auto-scroll to bottom
            container.scrollTop = container.scrollHeight;
        }
    }
};


// --- Utilities ---

function formatDuration(seconds) {
    const h = Math.floor(seconds / 3600);
    const m = Math.floor((seconds % 3600) / 60);
    const s = seconds % 60;

    if (h > 0) {
        return `${h}h ${m}m`;
    } else if (m > 0) {
        return `${m}m ${s}s`;
    }
    return `${s}s`;
}

function escapeHtml(text) {
    const div = document.createElement('div');
    div.textContent = text;
    return div.innerHTML;
}

function formatDate(isoString) {
    if (!isoString) return '';
    const date = new Date(isoString);
    return date.toLocaleDateString('ru-RU', {
        day: '2-digit',
        month: '2-digit',
        year: 'numeric',
        hour: '2-digit',
        minute: '2-digit'
    });
}

function escapeRegex(str) {
    return str.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

function copyToClipboard() {
    const meeting = views.meeting;
    if (!meeting.meta || !meeting.segments) return;

    const date = new Date(meeting.meta.created_at);
    const dateStr = date.toLocaleDateString('ru-RU', {
        day: 'numeric',
        month: 'long',
        year: 'numeric'
    });

    let md = `# ${meeting.meta.title || 'Untitled Meeting'}\n\n`;
    md += `**Date:** ${dateStr}\n\n`;
    md += `---\n\n`;

    const grouped = meeting.groupSegments(meeting.segments);
    for (const group of grouped) {
        md += `**${group.speaker}** *[${group.ts_display}]*: ${group.text}\n\n`;
    }

    navigator.clipboard.writeText(md).then(() => {
        // Show brief feedback
        const btn = document.querySelector('.meeting-actions .btn');
        const originalText = btn.textContent;
        btn.textContent = 'Copied!';
        setTimeout(() => btn.textContent = originalText, 1500);
    }).catch(err => {
        console.error('Copy failed:', err);
    });
}

function exportMarkdown() {
    const meeting = views.meeting;
    if (!meeting.meta || !meeting.segments) return;

    const date = new Date(meeting.meta.created_at);
    const dateStr = date.toLocaleDateString('ru-RU', {
        day: 'numeric',
        month: 'long',
        year: 'numeric'
    });

    let md = `# ${meeting.meta.title || 'Untitled Meeting'}\n\n`;
    md += `**Date:** ${dateStr}\n\n`;
    md += `---\n\n`;

    const grouped = meeting.groupSegments(meeting.segments);
    for (const group of grouped) {
        md += `**${group.speaker}** *[${group.ts_display}]*: ${group.text}\n\n`;
    }

    // Download
    const blob = new Blob([md], { type: 'text/markdown' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `${meeting.id}.md`;
    a.click();
    URL.revokeObjectURL(url);
}


// --- AI Analysis ---

let aiResponses = {};  // Store responses for each provider
let aiProviderNames = {};  // Store provider display names
let activeAiTab = 'claude';
let aiEventSource = null;

async function analyzeIncrement() {
    const meeting = views.meeting;
    if (!meeting.id) return;

    // Close any existing connection
    if (aiEventSource) {
        aiEventSource.close();
        aiEventSource = null;
    }

    // Reset state
    aiResponses = {};
    aiProviderNames = {};
    activeAiTab = 'claude';

    // Show modal with loading states
    document.getElementById('ai-modal').style.display = 'flex';
    document.getElementById('ai-response-claude').innerHTML =
        '<div class="loading">Connecting to Claude...</div>';
    document.getElementById('ai-response-codex').innerHTML =
        '<div class="loading">Connecting to Codex...</div>';

    // Reset tabs
    document.querySelectorAll('.tab-btn').forEach(btn => btn.classList.remove('active'));
    document.querySelector('.tab-btn[data-provider="claude"]').classList.add('active');
    document.querySelectorAll('.ai-tab-content').forEach(c => c.classList.remove('active'));
    document.getElementById('ai-response-claude').classList.add('active');

    // Use SSE for streaming
    const url = `/api/meetings/${meeting.id}/analyze-stream?question=increment`;
    aiEventSource = new EventSource(url);

    aiEventSource.onmessage = (event) => {
        try {
            const data = JSON.parse(event.data);
            handleAiStreamEvent(data);
        } catch (e) {
            console.error('Failed to parse SSE event:', e);
        }
    };

    aiEventSource.onerror = (e) => {
        console.error('SSE error:', e);
        aiEventSource.close();
        aiEventSource = null;

        // Show error if no responses received
        if (Object.keys(aiResponses).length === 0) {
            document.getElementById('ai-response-claude').innerHTML =
                '<div class="error">Connection error</div>';
            document.getElementById('ai-response-codex').innerHTML =
                '<div class="error">Connection error</div>';
        }
    };
}

function handleAiStreamEvent(data) {
    const { type, provider, name, text, message } = data;

    switch (type) {
        case 'start':
            // Provider started
            aiProviderNames[provider] = name;
            const startContainer = document.getElementById(`ai-response-${provider}`);
            if (startContainer) {
                startContainer.innerHTML = '<div class="loading">Analyzing...</div>';
            }
            break;

        case 'chunk':
            // Partial response
            aiResponses[provider] = text;
            updateAiResponseDisplay(provider, text, false);
            break;

        case 'done':
            // Final response from provider
            aiResponses[provider] = text;
            updateAiResponseDisplay(provider, text, true);
            break;

        case 'error':
            // Error from provider
            if (provider) {
                const errorContainer = document.getElementById(`ai-response-${provider}`);
                if (errorContainer) {
                    errorContainer.innerHTML = `<div class="error">Error: ${escapeHtml(message)}</div>`;
                }
            } else {
                // Global error
                document.getElementById('ai-response-claude').innerHTML =
                    `<div class="error">Error: ${escapeHtml(message)}</div>`;
                document.getElementById('ai-response-codex').innerHTML =
                    `<div class="error">Error: ${escapeHtml(message)}</div>`;
            }
            break;

        case 'complete':
            // All providers done
            if (aiEventSource) {
                aiEventSource.close();
                aiEventSource = null;
            }
            break;
    }
}

function updateAiResponseDisplay(provider, text, isDone) {
    const container = document.getElementById(`ai-response-${provider}`);
    if (!container) return;

    const providerName = aiProviderNames[provider] || provider;
    const statusIndicator = isDone ? '' : '<span class="streaming-indicator">●</span> ';
    const formattedText = renderMarkdown(text);

    container.innerHTML =
        `<div class="ai-answer">${statusIndicator}${formattedText}</div>
         <div class="ai-provider">via ${escapeHtml(providerName)}</div>`;
}

function renderMarkdown(text) {
    // Simple markdown renderer for headers, lists, bold, italic
    let html = escapeHtml(text);

    // Headers: ## Header
    html = html.replace(/^## (.+)$/gm, '<h3>$1</h3>');
    html = html.replace(/^### (.+)$/gm, '<h4>$1</h4>');

    // Bold: **text** or __text__
    html = html.replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>');
    html = html.replace(/__(.+?)__/g, '<strong>$1</strong>');

    // Italic: *text* or _text_
    html = html.replace(/\*([^*]+)\*/g, '<em>$1</em>');
    html = html.replace(/_([^_]+)_/g, '<em>$1</em>');

    // Code: `code`
    html = html.replace(/`([^`]+)`/g, '<code>$1</code>');

    // Lists: - item or * item
    html = html.replace(/^- (.+)$/gm, '<li>$1</li>');
    html = html.replace(/^\* (.+)$/gm, '<li>$1</li>');

    // Wrap consecutive <li> in <ul>
    html = html.replace(/(<li>.*<\/li>\n?)+/g, '<ul>$&</ul>');

    // Line breaks for remaining text
    html = html.replace(/\n/g, '<br>');

    // Clean up extra <br> after block elements
    html = html.replace(/<\/h3><br>/g, '</h3>');
    html = html.replace(/<\/h4><br>/g, '</h4>');
    html = html.replace(/<\/ul><br>/g, '</ul>');
    html = html.replace(/<br><ul>/g, '<ul>');

    return html;
}

function switchAiTab(provider) {
    activeAiTab = provider;

    // Update tab buttons
    document.querySelectorAll('.tab-btn').forEach(btn => {
        btn.classList.toggle('active', btn.dataset.provider === provider);
    });

    // Update tab content
    document.querySelectorAll('.ai-tab-content').forEach(content => {
        content.classList.toggle('active', content.id === `ai-response-${provider}`);
    });
}

function closeAiModal() {
    // Close SSE connection if active
    if (aiEventSource) {
        aiEventSource.close();
        aiEventSource = null;
    }
    document.getElementById('ai-modal').style.display = 'none';
}

function copyAiResponse() {
    // Copy raw markdown from aiResponses, not rendered HTML
    const text = aiResponses[activeAiTab];
    if (text) {
        navigator.clipboard.writeText(text).then(() => {
            const btn = document.querySelector('.modal-footer .btn');
            const originalText = btn.textContent;
            btn.textContent = 'Copied!';
            setTimeout(() => btn.textContent = originalText, 1500);
        });
    }
}


// --- Init ---

document.addEventListener('DOMContentLoaded', () => {
    router.init();
});
