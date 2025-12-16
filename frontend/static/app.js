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
        filterSource: '',

        async load() {
            const container = document.getElementById('meetings-list');
            container.innerHTML = '<div class="loading">Loading meetings...</div>';

            try {
                this.meetings = await api.getMeetings();
                this.render();
            } catch (e) {
                container.innerHTML = `<div class="error">Error: ${e.message}</div>`;
            }

            // Setup filter
            document.getElementById('filter-source').addEventListener('change', (e) => {
                this.filterSource = e.target.value;
                this.render();
            });
        },

        render() {
            const container = document.getElementById('meetings-list');
            let filtered = this.meetings;

            if (this.filterSource) {
                filtered = this.meetings.filter(m => m.source === this.filterSource);
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

            return `
                <a href="#/meeting/${meeting.id}" class="meeting-card">
                    <div class="meeting-card-header">
                        <span class="meeting-source ${sourceClass}">${sourceLabel}</span>
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

function escapeRegex(str) {
    return str.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
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


// --- Init ---

document.addEventListener('DOMContentLoaded', () => {
    router.init();
});
