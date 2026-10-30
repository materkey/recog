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
    },

    async generateTitle(id) {
        const res = await fetch(`/api/meetings/${id}/generate-title`, {
            method: 'POST'
        });
        if (!res.ok) {
            const error = await res.json().catch(() => ({}));
            throw new Error(error.detail || 'Failed to generate title');
        }
        const result = await res.json();
        views.meetings.invalidate();
        return result;
    },

    async deleteMeeting(id) {
        const res = await fetch(`/api/meetings/${id}`, { method: 'DELETE' });
        if (!res.ok) {
            const error = await res.json().catch(() => ({}));
            throw new Error(error.detail || 'Failed to delete meeting');
        }
        const result = await res.json();
        views.meetings.invalidate();
        return result;
    },

    async getPrompts() {
        const res = await fetch('/api/prompts');
        if (!res.ok) throw new Error('Failed to load prompts');
        return res.json();
    },

    async getIncrements(meetingId) {
        const res = await fetch(`/api/meetings/${meetingId}/increments`);
        if (!res.ok) throw new Error('Failed to load saved results');
        return res.json();
    },

    async getIncrement(meetingId, promptId, version) {
        const res = await fetch(`/api/meetings/${meetingId}/increments/${encodeURIComponent(promptId)}/${version}`);
        if (!res.ok) return null;
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

const meetingListCache = {
    key: 'recog.meetings.v1',
    maxAgeMs: 5 * 60 * 1000,

    read() {
        try {
            const cached = JSON.parse(sessionStorage.getItem(this.key));
            const age = Date.now() - cached?.savedAt;
            if (!Number.isFinite(age) || age < 0 || age > this.maxAgeMs) return null;
            if (!Array.isArray(cached.meetings) || !cached.meetings.every(m =>
                m && typeof m.id === 'string' && typeof m.created_at === 'string' &&
                (m.title == null || typeof m.title === 'string') &&
                (m.participants == null || Array.isArray(m.participants))
            )) return null;
            return cached.meetings;
        } catch {
            return null;
        }
    },

    write(meetings) {
        try {
            sessionStorage.setItem(this.key, JSON.stringify({ savedAt: Date.now(), meetings }));
        } catch { /* Storage can be disabled or full; the in-memory list still works. */ }
    },

    clear() {
        try { sessionStorage.removeItem(this.key); } catch { /* Optional cache. */ }
    }
};

// Display names for the sources recog records itself. An installation names
// the sources of its own importers in [sources.labels] of its config.toml,
// which the server sends as each meeting's source_label; any other source is
// shown as is.
const SOURCE_LABELS = {
    ktalk: 'KTalk',
    vas3k: 'vas3k',
    jitsi: 'Jitsi',
    matrix: 'Matrix',
    gmeet: 'Google Meet',
    zoom: 'Zoom',
    orphan: 'Orphan',
    local: 'Local',
    diarized: 'Diarized',
};

function sourceLabels(meetings) {
    const labels = { ...SOURCE_LABELS };
    for (const m of meetings) {
        if (m.source_label) labels[m.source] = m.source_label;
    }
    return labels;
}

const views = {
    meetings: {
        meetings: [],
        filterSource: localStorage.getItem('filterSource') ?? 'ktalk',
        filterTitle: localStorage.getItem('filterTitle') ?? '',
        searchMode: localStorage.getItem('searchMode') ?? 'title',
        searchTimeout: null,
        _listenersInitialized: false,
        _hasLoaded: false,
        _loadId: 0,

        invalidate() {
            // A GET started before a mutation must not repopulate either cache.
            this._loadId++;
            this._hasLoaded = false;
            this.meetings = [];
            meetingListCache.clear();
            if ((window.location.hash.slice(1) || '/') === '/') this.load();
        },

        async load() {
            const loadId = ++this._loadId;
            const container = document.getElementById('meetings-list');
            const status = document.getElementById('meetings-status');
            const saved = this._hasLoaded ? this.meetings : meetingListCache.read();
            container.setAttribute('aria-busy', 'true');
            status.textContent = saved !== null ? 'Updating saved meetings…' : '';

            // Restore filter values to UI
            const sourceSelect = document.getElementById('filter-source');
            const titleInput = document.getElementById('filter-title');
            const searchModeSelect = document.getElementById('search-mode');
            this.renderSourceOptions();
            titleInput.value = this.filterTitle;
            searchModeSelect.value = this.searchMode;

            // Init date picker
            DatePicker.init('filter-date', () => this.render());

            if (saved !== null) {
                this.meetings = saved;
                this.renderSourceOptions();
                this.render();
            } else {
                container.innerHTML = '<div class="loading">Loading meetings...</div>';
            }

            // Setup filter listeners only once (prevents duplicates on view re-entry)
            if (!this._listenersInitialized) {
                this._listenersInitialized = true;

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
            }

            try {
                const meetings = await api.getMeetings();
                if (loadId !== this._loadId) return;
                this.meetings = meetings;
                this._hasLoaded = true;
                this.renderSourceOptions();
                this.render();
                status.textContent = '';
                meetingListCache.write(meetings);
            } catch (e) {
                if (loadId !== this._loadId) return;
                if (saved !== null) {
                    status.textContent = 'Could not refresh meetings. Showing saved list.';
                } else {
                    container.innerHTML = `<div class="error">Error: ${escapeHtml(e.message)}</div>`;
                }
            } finally {
                if (loadId === this._loadId) container.removeAttribute('aria-busy');
            }
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
                                <span class="match-text">${highlightSearch(m.text, this.filterTitle)}</span>
                            </div>
                        `).join('')}
                    </div>
                </div>
            `).join('');
        },

        // Source options come from the meetings actually stored, most frequent
        // first. The saved selection stays listed even when no meeting has it,
        // so the select never silently shows a different filter than applied.
        renderSourceOptions() {
            const counts = new Map();
            for (const m of this.meetings) {
                const source = m.source || 'unknown';
                counts.set(source, (counts.get(source) || 0) + 1);
            }
            const labels = sourceLabels(this.meetings);
            if (this.filterSource && this.filterSource !== 'active' && !counts.has(this.filterSource)) {
                counts.set(this.filterSource, 0);
            }
            const sources = [...counts.keys()].sort((a, b) =>
                counts.get(b) - counts.get(a) || a.localeCompare(b));
            const select = document.getElementById('filter-source');
            select.innerHTML = [
                '<option value="">All sources</option>',
                '<option value="active">Active (LIVE)</option>',
                ...sources.map(source =>
                    `<option value="${escapeHtml(source)}">${escapeHtml(labels[source] || source)}</option>`),
            ].join('');
            select.value = this.filterSource;
        },

        render() {
            const container = document.getElementById('meetings-list');
            let filtered = this.meetings;

            if (this.filterTitle) {
                // Search spans the whole base: date/source narrow browsing, not search
                // (matches the by-content mode, which never applied them)
                const term = this.filterTitle.toLowerCase();
                filtered = filtered.filter(m =>
                    (m.title || '').toLowerCase().includes(term)
                );
            } else {
                // Filter by date
                filtered = DatePicker.filterMeetings(filtered, DatePicker.getDate());

                // Filter by source
                if (this.filterSource) {
                    if (this.filterSource === 'active') {
                        filtered = filtered.filter(m => m.is_active);
                    } else {
                        filtered = filtered.filter(m => (m.source || 'unknown') === this.filterSource);
                    }
                }
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
        _searchListenerInitialized: false,
        privacyMode: localStorage.getItem('meetingPrivacyMode') === 'true',

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

                // Hide Delete while the meeting is still recording (ended_at === null)
                const delBtn = document.getElementById('delete-meeting-btn');
                if (delBtn) delBtn.style.display = meta.ended_at ? '' : 'none';

                // Sync privacy toggle button with persisted state
                syncPrivacyButtons();

                // Setup search (only once)
                if (!this._searchListenerInitialized) {
                    this._searchListenerInitialized = true;
                    document.getElementById('transcript-search').addEventListener('input', (e) => {
                        this.filterTranscript(e.target.value);
                    });
                }
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

            // Show generate title button for orphan meetings without title
            const showGenerateTitle = !meta.title && meta.source === 'orphan';

            // Audio tracks
            const hasMicAudio = meta.audio_mic_path && !meta.audio_deleted_at;
            const hasSystemAudio = meta.audio_system_path && !meta.audio_deleted_at;

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
                ${showGenerateTitle ? `
                    <div class="meta-title-actions">
                        <button class="btn btn-generate-title" onclick="generateTitle()">
                            Generate Title (Codex)
                        </button>
                    </div>
                ` : ''}
                ${(hasMicAudio || hasSystemAudio) ? `
                    <div class="meta-audio-tracks">
                        <span class="meta-label">Audio Tracks:</span>
                        <div class="audio-track-list">
                            ${hasMicAudio ? `
                                <div class="audio-track">
                                    <span class="track-icon">🎤</span>
                                    <span class="track-name">Mic</span>
                                </div>
                            ` : ''}
                            ${hasSystemAudio ? `
                                <div class="audio-track">
                                    <span class="track-icon">🔊</span>
                                    <span class="track-name">System</span>
                                </div>
                            ` : ''}
                        </div>
                    </div>
                ` : ''}
            `;
        },

        renderTranscript(segments, container, searchTerm = '') {
            if (segments.length === 0) {
                container.innerHTML = '<div class="empty">No transcript segments</div>';
                return;
            }

            // Group by speaker, preserving segment indices for audio playback
            const grouped = this.groupSegments(segments);

            container.className = 'transcript';

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
                    ? highlightSearch(group.text, searchTerm)
                    : escapeHtml(group.text);

                // Check if segment has audio data
                const hasAudio = group.segmentIndices && group.segmentIndices.some(idx => {
                    const seg = segments[idx];
                    return seg && seg.audio_offset_ms !== undefined;
                });
                const segmentIndices = group.segmentIndices ? group.segmentIndices.join(',') : '';

                // Play button for segments with audio (only first segment in group)
                const firstSegmentIdx = group.segmentIndices ? group.segmentIndices[0] : null;
                const playButton = hasAudio && firstSegmentIdx !== null ? `
                    <button class="btn-play-segment" onclick="playSegmentAudio('${this.id}', ${firstSegmentIdx}, this)" title="Play audio">
                        <svg class="play-icon" viewBox="0 0 24 24" width="16" height="16">
                            <polygon points="5,3 19,12 5,21" fill="currentColor"/>
                        </svg>
                    </button>
                ` : '';

                return `
                    <div class="transcript-segment ${speakerClass}"
                         data-segment-indices="${segmentIndices}">
                        <div class="segment-content">
                            <div class="segment-header">
                                <span class="segment-speaker">${escapeHtml(group.speaker)}</span>
                                ${playButton}
                                <span class="segment-time">${group.ts_display}</span>
                            </div>
                            <div class="segment-text">${text}</div>
                        </div>
                    </div>
                `;
            }).join('');
        },

        groupSegments(segments) {
            const groups = [];
            let current = null;

            for (let i = 0; i < segments.length; i++) {
                const seg = segments[i];
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
                        event_type: seg.event_type,
                        segmentIndices: [i]
                    });
                    continue;
                }

                if (current && current.speaker_id === seg.speaker_id && seg.cont) {
                    current.text += ' ' + seg.text;
                    current.segmentIndices.push(i);
                } else {
                    if (current) groups.push(current);
                    current = {
                        speaker: seg.speaker,
                        speaker_id: seg.speaker_id,
                        is_local: seg.is_local,
                        ts_display: seg.ts_display,
                        text: seg.text,
                        segmentIndices: [i]
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
        }
    },

    live: {
        ws: null,
        segments: [],
        currentMeeting: null,
        refineEnabled: false,
        followMode: localStorage.getItem('liveFollowMode') !== 'false',
        pendingRefines: new Map(),

        connect() {
            this.updateFollowButton();
            syncPrivacyButtons();
            const statusEl = document.getElementById('live-status');
            const infoEl = document.getElementById('live-meeting-info');
            const transcriptEl = document.getElementById('live-transcript');

            this.updateStatus('connecting');
            transcriptEl.innerHTML = '';

            const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
            const ws = new WebSocket(`${protocol}//${window.location.host}/api/live/stream`);
            this.ws = ws;

            ws.onopen = () => {
                if (this.ws !== ws) return;
                this.updateStatus('connected');
            };

            ws.onclose = () => {
                if (this.ws !== ws) return;
                this.ws = null;
                this.updateStatus('disconnected');
                copilotHandleWsClosed();
            };

            ws.onerror = () => {
                if (this.ws !== ws) return;
                this.updateStatus('error');
            };

            ws.onmessage = (event) => {
                if (this.ws !== ws) return;
                const msg = JSON.parse(event.data);
                this.handleMessage(msg);
            };
        },

        disconnect() {
            const ws = this.ws;
            this.ws = null;
            if (ws) ws.close();
            copilotHandleWsClosed();
            this.segments = [];
            this.currentMeeting = null;
            this.pendingRefines.clear();
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
                case 'privacy_updated':
                    applyPrivacyState(msg);
                    break;
                case 'init':
                    this.currentMeeting = msg.meeting;
                    this.segments = msg.segments || [];
                    this.refineEnabled = msg.refine_enabled === true;
                    // Do NOT clear pendingRefines here: a `transcript_refine`
                    // forwarded by the bridge can arrive BEFORE the init
                    // handshake completes. Clearing would drop that update,
                    // leaving the tier-1 row stuck even though backend
                    // emitted a tier-2 refine. `appendSegments` below
                    // consumes matching entries per segment id and leaves
                    // non-matching entries for future `segments` events.
                    // Cross-meeting cleanup: a WS reconnect whose init
                    // lands on a different meeting (meeting rotated while
                    // we were offline) must not leak the previous
                    // meeting's pending refines, since segment_ids restart
                    // at 1 per meeting and would alias.
                    this.prunePendingRefinesForMeeting(msg.meeting ? msg.meeting.id : null);
                    this.renderMeetingInfo(msg.meeting, infoEl);
                    this.renderTranscript(transcriptEl);
                    updateCopilotControls();
                    break;

                case 'meeting_change':
                    this.currentMeeting = msg.meeting;
                    this.refineEnabled = msg.refine_enabled === true;
                    this.segments = [];
                    this.pendingRefines.clear();
                    this.renderMeetingInfo(msg.meeting, infoEl);
                    this.renderTranscript(transcriptEl);
                    updateCopilotControls();
                    break;

                case 'segments':
                    this.segments.push(...msg.data);
                    this.appendSegments(msg.data, transcriptEl);
                    break;

                case 'refine_state':
                    // In-place orphan -> KTalk promotion: update the refine
                    // flag so newly appended segments get the tier-1 cue,
                    // without clearing the rendered transcript.
                    if (!msg.meeting_id || !this.currentMeeting
                            || msg.meeting_id === this.currentMeeting.id) {
                        this.refineEnabled = msg.refine_enabled === true;
                        if (this.currentMeeting && this.refineEnabled) {
                            this.currentMeeting.source = 'ktalk';
                            updateCopilotControls();
                        }
                    }
                    break;

                case 'transcript_refine':
                    this.handleTranscriptRefine(msg, transcriptEl);
                    break;

                case 'copilot_reply':
                case 'copilot_chat_echo':
                    handleCopilotDialogFrame(msg);
                    break;

                case 'copilot_history':
                    handleCopilotHistory(msg);
                    break;

                case 'copilot_status':
                    handleCopilotStatus(msg);
                    break;

                case 'user_profile_status':
                    window.userProfile?.refresh();
                    break;

                case 'error':
                    infoEl.innerHTML = `<div class="error">${msg.message}</div>`;
                    break;
            }
        },

        handleTranscriptRefine(msg, container) {
            const currentId = this.currentMeeting ? this.currentMeeting.id : null;
            if (msg && msg.meeting_id && currentId && msg.meeting_id !== currentId) {
                return;
            }
            const updates = Array.isArray(msg && msg.updates) ? msg.updates : [];
            const refineMeetingId = msg && msg.meeting_id ? msg.meeting_id : currentId;
            for (const update of updates) {
                if (update == null || typeof update !== 'object') continue;
                const segmentId = update.segment_id;
                const text = update.text;
                if (typeof segmentId !== 'number' || segmentId <= 0) continue;
                if (typeof text !== 'string') continue;

                for (let i = 0; i < this.segments.length; i++) {
                    if (this.segments[i].id === segmentId) {
                        this.segments[i].text = text;
                        this.segments[i].refined = true;
                        break;
                    }
                }

                const row = container && container.querySelector
                    ? container.querySelector(`[data-segment-id="${segmentId}"]`)
                    : null;
                if (row) {
                    const textEl = row.querySelector('.segment-text');
                    if (textEl) {
                        textEl.textContent = text;
                    }
                    row.dataset.refined = 'true';
                    row.title = '';
                } else {
                    // Tag with meeting_id so a WS reconnect that lands on
                    // a different meeting cannot consume this entry against
                    // an unrelated segment sharing the same id (ids restart
                    // at 1 per meeting).
                    this.pendingRefines.set(segmentId, {
                        text,
                        refined_at: msg.refined_at,
                        meeting_id: refineMeetingId,
                    });
                }
            }
        },

        prunePendingRefinesForMeeting(meetingId) {
            // Drop pending entries whose meeting_id does not match the
            // currently-displayed meeting. Legacy entries without a
            // meeting_id (should not happen, defensive) are kept.
            if (!meetingId) return;
            for (const [segId, entry] of this.pendingRefines) {
                if (entry && entry.meeting_id && entry.meeting_id !== meetingId) {
                    this.pendingRefines.delete(segId);
                }
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
                if (seg.id != null && this.pendingRefines.has(seg.id)) {
                    const pending = this.pendingRefines.get(seg.id);
                    seg.text = pending.text;
                    seg.refined = true;
                    this.pendingRefines.delete(seg.id);
                }

                const speakerClass = seg.is_local ? 'speaker-local' : 'speaker-other';

                const div = document.createElement('div');
                div.className = `transcript-segment ${speakerClass} segment-new`;
                div.dataset.tsDisplay = seg.ts_display || '';
                if (seg.id != null) {
                    div.dataset.segmentId = String(seg.id);
                }
                if (!seg.event_type && (this.refineEnabled || seg.refined === true)) {
                    div.dataset.refined = seg.refined === true ? 'true' : 'false';
                    if (div.dataset.refined === 'false') {
                        div.title = 'Предварительная расшифровка — будет уточнена';
                    }
                }

                // Audio playback: the init frame delivers the same full
                // segment list the /audio/{segment_index} endpoint indexes
                // into, and `segments` frames append rows in file order, so
                // the position in this.segments is the API's segment_index.
                const hasAudio = !seg.event_type && seg.audio_offset_ms != null;
                const playButton = hasAudio ? `
                    <button class="btn-play-segment" title="Play audio">
                        <svg class="play-icon" viewBox="0 0 24 24" width="16" height="16">
                            <polygon points="5,3 19,12 5,21" fill="currentColor"/>
                        </svg>
                    </button>
                ` : '';

                div.innerHTML = `
                    <div class="segment-header">
                        <span class="segment-speaker">${escapeHtml(seg.speaker)}</span>
                        ${playButton}
                        <span class="segment-time">${seg.ts_display}</span>
                    </div>
                    <div class="segment-text">${escapeHtml(seg.text)}</div>
                `;

                const playBtn = div.querySelector('.btn-play-segment');
                if (playBtn) {
                    const segmentIndex = this.segments.indexOf(seg);
                    playBtn.addEventListener('click', () => {
                        const meetingId = this.currentMeeting && this.currentMeeting.id;
                        playSegmentAudio(meetingId, segmentIndex, playBtn);
                    });
                }

                container.appendChild(div);

                // Remove animation class after animation
                setTimeout(() => div.classList.remove('segment-new'), 500);
            }

            // Auto-scroll to bottom if follow mode is on
            if (this.followMode) {
                container.scrollTop = container.scrollHeight;
            }
        },

        updateFollowButton() {
            const btn = document.getElementById('follow-btn');
            if (btn) {
                btn.classList.toggle('active', this.followMode);
            }
        },

        toggleFollow() {
            this.followMode = !this.followMode;
            localStorage.setItem('liveFollowMode', this.followMode);
            this.updateFollowButton();

            // Scroll to bottom immediately when enabling
            if (this.followMode) {
                const container = document.getElementById('live-transcript');
                if (container) {
                    container.scrollTop = container.scrollHeight;
                }
            }
        }
    }
};

function toggleFollowMode() {
    views.live.toggleFollow();
}


// --- Copilot Panel ---
// Module-level state so chat history, chip state, and pending requests
// survive view switches (views.live.disconnect() must not clear this).
// The conversation itself is shared: the server replays it to every new
// connection, so F5 and a new tab get it back (local error bubbles stay local).
// The session survives tab close (D2).

const COPILOT_TRANSCRIPT_WINDOW_SIZE = 15;

// Manual Stop is an immediate kill at the supervisor (~2-5 s to ⚫ offline).
// If no offline arrives within this window the stop frame was lost somewhere
// on the webapp → bridge → supervisor path — surface it instead of showing
// "останавливается…" forever (2026-08-06 incident).
const COPILOT_STOP_CONFIRM_TIMEOUT_MS = 15000;

const COPILOT_AGENT_CHIP = {
    online: { icon: '🟢', label: 'online', cls: 'online' },
    working: { icon: '🟡', label: 'working', cls: 'working' },
    spawning: { icon: '🟡', label: 'spawning', cls: 'working' },
    blocked: { icon: '🔴', label: 'blocked', cls: 'blocked' },
    offline: { icon: '⚫', label: 'offline', cls: 'offline' }
};

const COPILOT_CONFERENCE_SOURCES = new Set(['ktalk', 'vas3k', 'jitsi', 'matrix', 'gmeet', 'zoom']);

const copilotState = {
    messages: [],           // {id, kind, text, request_id, status, time}
    pending: new Map(),     // request_id -> {userId, assistantId, createdAt}
    seenDialogIds: new Set(),   // dialog_id of the frames already shown
    agentState: 'offline',
    panelOpen: false,
    unread: false,
    spawnPendingId: null,   // request_id of the in-flight spawn click
    stopPendingId: null     // request_id of the in-flight stop click
};

function copilotTimeDisplay(ms) {
    return new Date(ms).toLocaleTimeString('ru-RU', {
        hour: '2-digit',
        minute: '2-digit',
        second: '2-digit'
    });
}

function copilotNowDisplay() {
    return copilotTimeDisplay(Date.now());
}

function generateCopilotRequestId() {
    return 'copilot_' + crypto.randomUUID();
}

function nextCopilotMsgId() {
    return 'copilot-msg-' + Date.now().toString(36) + '-' + Math.random().toString(36).slice(2, 8);
}

// Single OPEN-guarded send helper: every copilot frame goes through here.
function sendCopilotFrame(frame) {
    const ws = views.live.ws;
    if (!ws || ws.readyState !== WebSocket.OPEN) return false;
    ws.send(JSON.stringify(frame));
    return true;
}

function copilotRoleLabel(kind) {
    if (kind === 'user') return 'Вы';
    if (kind === 'assistant') return 'Claude';
    if (kind === 'proactive') return 'Claude · proactive';
    if (kind === 'status') return 'Status';
    if (kind === 'error') return 'Error';
    return kind;
}

function getCopilotMessagesEl() {
    return document.getElementById('copilot-messages');
}

function scrollCopilotToBottom() {
    const messagesEl = getCopilotMessagesEl();
    if (messagesEl) messagesEl.scrollTop = messagesEl.scrollHeight;
}

// Text content of a bubble: assistant/proactive kinds render through the
// escape-first renderMarkdown (safe for innerHTML) with .ai-answer
// typography; user/error/status and pending placeholders stay plain text.
function setCopilotMsgText(textEl, msg) {
    const isPending = msg.kind === 'assistant'
        && typeof msg.status === 'string'
        && msg.status !== 'done'
        && msg.status !== 'error';
    const isMarkdownKind = msg.kind === 'assistant' || msg.kind === 'proactive';
    textEl.className = 'copilot-msg-text'
        + (isPending ? ' pending' : '')
        + (isMarkdownKind && !isPending ? ' ai-answer' : '');
    if (isMarkdownKind && !isPending) {
        textEl.innerHTML = renderMarkdown(msg.text || '');
    } else {
        textEl.textContent = msg.text || (isPending ? '…' : '');
    }
}

function renderCopilotMessage(msg) {
    const messagesEl = getCopilotMessagesEl();
    if (!messagesEl) return null;
    const empty = messagesEl.querySelector('.copilot-empty');
    if (empty) empty.remove();

    const wrapper = document.createElement('div');
    wrapper.className = `copilot-msg copilot-msg-${msg.kind}`;
    wrapper.dataset.copilotId = msg.id;
    if (msg.request_id) wrapper.dataset.copilotRequestId = msg.request_id;

    const header = document.createElement('div');
    header.className = 'copilot-msg-header';
    const roleEl = document.createElement('span');
    roleEl.className = 'copilot-msg-role';
    roleEl.textContent = copilotRoleLabel(msg.kind);
    const timeEl = document.createElement('span');
    timeEl.className = 'copilot-msg-time';
    timeEl.textContent = msg.time || copilotNowDisplay();
    header.appendChild(roleEl);
    header.appendChild(timeEl);

    const textEl = document.createElement('div');
    setCopilotMsgText(textEl, msg);

    wrapper.appendChild(header);
    wrapper.appendChild(textEl);
    messagesEl.appendChild(wrapper);
    scrollCopilotToBottom();
    return wrapper;
}

// Append-only insertion with scroll-to-bottom; returns the message id.
function appendCopilotMessage(msg) {
    if (!msg.id) msg.id = nextCopilotMsgId();
    if (!msg.time) msg.time = copilotNowDisplay();
    copilotState.messages.push(msg);
    renderCopilotMessage(msg);
    noteCopilotUnread(msg.kind);
    return msg.id;
}

// In-place mutation of an existing bubble (no reorder).
function updateCopilotMessage(id, updates) {
    const msg = copilotState.messages.find((m) => m.id === id);
    if (!msg) return;
    Object.assign(msg, updates);

    const messagesEl = getCopilotMessagesEl();
    const el = messagesEl ? messagesEl.querySelector(`[data-copilot-id="${id}"]`) : null;
    if (!el) return;
    if (updates.kind) {
        el.classList.remove(
            'copilot-msg-user',
            'copilot-msg-assistant',
            'copilot-msg-proactive',
            'copilot-msg-status',
            'copilot-msg-error'
        );
        el.classList.add(`copilot-msg-${msg.kind}`);
        const roleEl = el.querySelector('.copilot-msg-role');
        if (roleEl) roleEl.textContent = copilotRoleLabel(msg.kind);
    }
    if (updates.time != null) {
        const timeEl = el.querySelector('.copilot-msg-time');
        if (timeEl) timeEl.textContent = msg.time;
    }
    const textEl = el.querySelector('.copilot-msg-text');
    if (textEl) setCopilotMsgText(textEl, msg);
}

function noteCopilotUnread(kind) {
    if (kind !== 'assistant' && kind !== 'proactive') return;
    if (copilotState.panelOpen) return;
    copilotState.unread = true;
    updateCopilotUnreadDot();
}

function updateCopilotUnreadDot() {
    const dot = document.getElementById('copilot-unread-dot');
    if (dot) dot.hidden = !copilotState.unread;
}

function updateCopilotChips() {
    const meta = COPILOT_AGENT_CHIP[copilotState.agentState] || COPILOT_AGENT_CHIP.offline;
    for (const id of ['copilot-chip', 'copilot-panel-chip']) {
        const el = document.getElementById(id);
        if (!el) continue;
        el.classList.remove('online', 'working', 'blocked', 'offline');
        el.classList.add(meta.cls);
        el.textContent = `${meta.icon} ${meta.label}`;
        el.title = `Copilot session: ${meta.label}`;
    }
}

function updateCopilotControls() {
    const input = document.getElementById('copilot-input');
    const sendBtn = document.getElementById('copilot-send-btn');
    const stopBtn = document.getElementById('copilot-stop-btn');
    const spawning = copilotState.spawnPendingId !== null
        || copilotState.agentState === 'spawning';
    if (input) {
        input.disabled = spawning;
        input.placeholder = spawning ? 'запускается…' : 'Спросить Claude...';
    }
    if (sendBtn) sendBtn.disabled = spawning;
    if (stopBtn) {
        const stopping = copilotState.stopPendingId !== null;
        const meetingSource = views.live.currentMeeting && views.live.currentMeeting.source;
        const conferenceOwned = COPILOT_CONFERENCE_SOURCES.has(meetingSource);
        // Manual Stop kills immediately — "останавливается…" resolves to ⚫
        // within seconds, or the timeout in copilotStopClick reports the
        // stop as lost. Disabled while a stop is already in flight (no
        // duplicate frames) and while there is no manual session to stop.
        stopBtn.textContent = stopping ? 'останавливается…' : 'Stop';
        stopBtn.disabled = stopping
            || copilotState.agentState === 'offline'
            || conferenceOwned;
        stopBtn.title = conferenceOwned
            ? 'Conference Copilot sessions are managed automatically'
            : '';
    }
}

function openCopilotPanel() {
    copilotState.panelOpen = true;
    const panel = document.getElementById('copilot-panel');
    if (panel) panel.style.display = 'flex';
    copilotState.unread = false;
    updateCopilotUnreadDot();
    updateCopilotControls();
    scrollCopilotToBottom();
}

function closeCopilotPanel() {
    copilotState.panelOpen = false;
    const panel = document.getElementById('copilot-panel');
    if (panel) panel.style.display = 'none';
}

function toggleCopilotPanel() {
    if (copilotState.panelOpen) {
        closeCopilotPanel();
    } else {
        openCopilotPanel();
    }
}

function copilotButtonClick() {
    // Chip ⚫ and no spawn in flight: open the panel and send exactly one
    // spawn request. Otherwise the button just toggles the panel.
    if (copilotState.agentState === 'offline' && !copilotState.spawnPendingId) {
        openCopilotPanel();
        const requestId = generateCopilotRequestId();
        if (!sendCopilotFrame({ type: 'copilot_spawn_request', request_id: requestId })) {
            appendCopilotMessage({
                kind: 'error',
                text: 'Bridge offline: сообщение не отправлено.'
            });
            return;
        }
        copilotState.spawnPendingId = requestId;
        updateCopilotControls();
        return;
    }
    toggleCopilotPanel();
}

function copilotStopClick() {
    if (copilotState.stopPendingId) return;
    const requestId = generateCopilotRequestId();
    if (!sendCopilotFrame({ type: 'copilot_stop_request', request_id: requestId })) {
        appendCopilotMessage({
            kind: 'error',
            text: 'Bridge offline: сообщение не отправлено.'
        });
        return;
    }
    copilotState.stopPendingId = requestId;
    updateCopilotControls();
    setTimeout(() => {
        if (copilotState.stopPendingId !== requestId) return;
        copilotState.stopPendingId = null;
        updateCopilotControls();
        appendCopilotMessage({
            kind: 'error',
            text: 'Stop не подтверждён за 15 с — запрос, похоже, потерялся. Попробуйте ещё раз.'
        });
    }, COPILOT_STOP_CONFIRM_TIMEOUT_MS);
}

// Last ≤15 non-event rows of the live transcript, in the ws_api.js wire
// shape ({speaker, text, timestamp, segment_id}).
function buildCopilotTranscriptWindow() {
    const segments = Array.isArray(views.live.segments) ? views.live.segments : [];
    return segments
        .filter((s) => s && !s.event_type)
        .slice(-COPILOT_TRANSCRIPT_WINDOW_SIZE)
        .map((s) => ({
            speaker: s.speaker || '',
            text: s.text || '',
            timestamp: s.ts_display || '',
            segment_id: s.id || 0
        }));
}

function submitCopilotChat() {
    const input = document.getElementById('copilot-input');
    if (!input || input.disabled) return;
    const text = (input.value || '').trim();
    if (!text) return;

    const meeting = views.live.currentMeeting;
    const requestId = generateCopilotRequestId();
    const sent = sendCopilotFrame({
        type: 'copilot_chat_message',
        request_id: requestId,
        text: text,
        meeting_id: (meeting && meeting.id) || null,
        meeting_title: (meeting && meeting.title) || null,
        transcript_window: buildCopilotTranscriptWindow(),
        created_at: Date.now()
    });
    if (!sent) {
        // WS not OPEN: error bubble, input untouched, no pending entry.
        appendCopilotMessage({
            kind: 'error',
            text: 'Bridge offline: сообщение не отправлено.'
        });
        return;
    }

    const userId = appendCopilotMessage({
        kind: 'user',
        text: text,
        request_id: requestId
    });
    const assistantId = appendCopilotMessage({
        kind: 'assistant',
        text: 'Жду ответа Claude…',
        request_id: requestId,
        status: 'working'
    });
    copilotState.pending.set(requestId, { userId, assistantId, createdAt: Date.now() });
    input.value = '';
}

// A frame of the shared dialog: live or replayed, each is shown once. A frontend
// service not yet restarted after an update serves this file but sends no ids.
function handleCopilotDialogFrame(frame) {
    if (frame.dialog_id) {
        if (copilotState.seenDialogIds.has(frame.dialog_id)) return;
        copilotState.seenDialogIds.add(frame.dialog_id);
    }
    if (frame.type === 'copilot_chat_echo') {
        handleCopilotChatEcho(frame);
    } else {
        handleCopilotReply(frame);
    }
}

// Replayed frames were read elsewhere or earlier: the dot stays as it was.
function handleCopilotHistory(data) {
    const unread = copilotState.unread;
    for (const frame of data.frames) {
        handleCopilotDialogFrame(frame);
    }
    copilotState.unread = unread;
    updateCopilotUnreadDot();
}

function copilotQuestionShown(requestId) {
    for (const msg of copilotState.messages) {
        if (msg.kind === 'user' && msg.request_id === requestId) return true;
    }
    return false;
}

// A question asked elsewhere. Its reply arrives as an ordinary append, since
// only the asking tab holds a placeholder; this tab's own question is on screen.
function handleCopilotChatEcho(data) {
    if (copilotQuestionShown(data.request_id)) return;
    appendCopilotMessage({
        kind: 'user',
        text: data.text,
        request_id: data.request_id,
        time: copilotTimeDisplay(data.created_at)
    });
}

function handleCopilotReply(data) {
    const requestId = data && data.request_id;
    const replyText = (data && (data.text || data.message)) || '';
    const kind = data && data.kind === 'proactive' ? 'proactive' : 'assistant';
    const time = copilotTimeDisplay(data.created_at);

    if (kind === 'proactive') {
        appendCopilotMessage({
            kind: 'proactive',
            text: replyText,
            request_id: requestId || null,
            time
        });
        return;
    }

    const pending = requestId ? copilotState.pending.get(requestId) : null;
    if (pending) {
        // Resolve the placeholder in place — no reorder.
        updateCopilotMessage(pending.assistantId, {
            kind: 'assistant',
            text: replyText,
            status: 'done',
            time
        });
        copilotState.pending.delete(requestId);
        noteCopilotUnread('assistant');
        return;
    }
    // Unknown request_id (e.g. extension-authored): append, never drop.
    appendCopilotMessage({
        kind: 'assistant',
        text: replyText,
        request_id: requestId || null,
        time
    });
}

// Drives the agent chip only — never touches per-request placeholders.
// Unknown values leave the chip unchanged.
function handleCopilotAgentState(next) {
    if (!COPILOT_AGENT_CHIP[next]) return;
    copilotState.agentState = next;
    // Spawn has no ack: pending clears once the chip leaves offline.
    if (copilotState.spawnPendingId && next !== 'offline') {
        copilotState.spawnPendingId = null;
    }
    // Stop is done only when the chip reaches offline.
    if (copilotState.stopPendingId && next === 'offline') {
        copilotState.stopPendingId = null;
    }
    updateCopilotChips();
    updateCopilotControls();
}

function handleCopilotStatus(data) {
    const status = (data && data.status) || 'unknown';
    const requestId = data && data.request_id;

    if (status === 'agent_state') {
        handleCopilotAgentState(data && data.agent_state);
        return;
    }

    if (status === 'channel_connected') {
        // Channel-level recovery signal — no dedicated indicator in the
        // webapp panel; must not fall through to placeholder handling.
        return;
    }

    if (status === 'conference_active') {
        // Spawn rejected: the bridge refuses manual spawn while a
        // conference meeting is active.
        if (requestId) {
            if (requestId !== copilotState.spawnPendingId) return;
            copilotState.spawnPendingId = null;
            updateCopilotControls();
        }
        appendCopilotMessage({ kind: 'status', text: 'занято конференцией' });
        return;
    }

    if (status === 'manual_preempted') {
        appendCopilotMessage({ kind: 'status', text: 'сессия переключена на конференцию' });
        return;
    }

    if (status === 'bridge_disconnected' || status === 'supervisor_disconnected') {
        const label = status === 'bridge_disconnected' ? 'Bridge offline' : 'Supervisor offline';
        if (requestId && requestId === copilotState.spawnPendingId) {
            // Spawn failed — button becomes clickable again (retry).
            copilotState.spawnPendingId = null;
            updateCopilotControls();
            appendCopilotMessage({ kind: 'error', text: `${label}: запрос не доставлен.` });
            return;
        }
        if (requestId && requestId === copilotState.stopPendingId) {
            // Stop failed — Stop stays enabled (retry affordance).
            copilotState.stopPendingId = null;
            updateCopilotControls();
            appendCopilotMessage({ kind: 'error', text: `${label}: запрос не доставлен.` });
            return;
        }
        const pending = requestId ? copilotState.pending.get(requestId) : null;
        if (pending) {
            updateCopilotMessage(pending.assistantId, {
                kind: 'error',
                text: `${label}: сообщение не доставлено.`,
                status: 'error',
                time: copilotNowDisplay()
            });
            copilotState.pending.delete(requestId);
            return;
        }
        if (requestId) {
            // Another tab's request-scoped failure.
            return;
        }
        appendCopilotMessage({ kind: 'error', text: `${label}: запрос не доставлен.` });
        return;
    }

    if (status === 'channel_disconnected') {
        // Defensive: a per-request frame carrying a spawn/stop id must not
        // leave the input bricked at 'запускается…'/'останавливается…'.
        if (requestId && requestId === copilotState.spawnPendingId) {
            copilotState.spawnPendingId = null;
            updateCopilotControls();
            appendCopilotMessage({
                kind: 'error',
                text: 'Channel offline: Claude не подключён.'
            });
            return;
        }
        if (requestId && requestId === copilotState.stopPendingId) {
            copilotState.stopPendingId = null;
            updateCopilotControls();
            appendCopilotMessage({
                kind: 'error',
                text: 'Channel offline: Claude не подключён.'
            });
            return;
        }
        const pending = requestId ? copilotState.pending.get(requestId) : null;
        if (pending) {
            updateCopilotMessage(pending.assistantId, {
                kind: 'error',
                text: 'Channel offline: Claude не подключён.',
                status: 'error',
                time: copilotNowDisplay()
            });
            copilotState.pending.delete(requestId);
            return;
        }
        if (requestId) {
            // Another tab's per-request echo fanned out through the shared
            // subscriber socket — nothing of ours to resolve, drop it.
            return;
        }
        if (copilotState.pending.size > 0) {
            // Channel died mid-flight with no specific request_id: flip
            // every stranded placeholder instead of one vague bubble.
            copilotState.pending.forEach((p) => {
                if (p && p.assistantId) {
                    updateCopilotMessage(p.assistantId, {
                        kind: 'error',
                        text: 'Channel offline: Claude не подключён.',
                        status: 'error',
                        time: copilotNowDisplay()
                    });
                }
            });
            copilotState.pending.clear();
            return;
        }
        appendCopilotMessage({
            kind: 'error',
            text: 'Channel offline: Claude не подключён.'
        });
        return;
    }

    if (status === 'error') {
        const message = (data && data.message) || 'Claude не смог ответить.';
        // Defensive: same spawn/stop unbricking as bridge/supervisor errors.
        if (requestId && requestId === copilotState.spawnPendingId) {
            copilotState.spawnPendingId = null;
            updateCopilotControls();
            appendCopilotMessage({ kind: 'error', text: message });
            return;
        }
        if (requestId && requestId === copilotState.stopPendingId) {
            copilotState.stopPendingId = null;
            updateCopilotControls();
            appendCopilotMessage({ kind: 'error', text: message });
            return;
        }
        const pending = requestId ? copilotState.pending.get(requestId) : null;
        if (pending) {
            updateCopilotMessage(pending.assistantId, {
                kind: 'error',
                text: message,
                status: 'error',
                time: copilotNowDisplay()
            });
            copilotState.pending.delete(requestId);
        } else {
            appendCopilotMessage({ kind: 'error', text: message });
        }
        return;
    }

    if (status === 'done') {
        // No-op if the reply already landed (pending gone by then).
        const pending = requestId ? copilotState.pending.get(requestId) : null;
        if (pending) {
            updateCopilotMessage(pending.assistantId, {
                kind: 'status',
                text: (data && data.message) || 'Запрос завершён без ответа.',
                status: 'done',
                time: copilotNowDisplay()
            });
            copilotState.pending.delete(requestId);
        }
        return;
    }

    // queued / working / unknown per-request status: mutate the matching
    // placeholder only; no bubble is created without a pending match.
    const pending = requestId ? copilotState.pending.get(requestId) : null;
    if (pending) {
        updateCopilotMessage(pending.assistantId, {
            status: status,
            text: status === 'working' ? 'Claude думает…' : `Status: ${status}`,
            time: copilotNowDisplay()
        });
    }
}

// WS closed (view switch or connection drop): a reply broadcast while this
// tab's socket is closed is permanently lost (no replay), so flip every
// in-flight placeholder to an error and reset spawn/stop pending state.
// Idempotent — views.live.disconnect() closing the ws also fires onclose,
// and the second run finds nothing pending.
function copilotHandleWsClosed() {
    if (copilotState.pending.size === 0
            && !copilotState.spawnPendingId
            && !copilotState.stopPendingId) {
        return;
    }
    copilotState.pending.forEach((p) => {
        if (p && p.assistantId) {
            updateCopilotMessage(p.assistantId, {
                kind: 'error',
                text: 'Bridge offline: соединение прервано.',
                status: 'error',
                time: copilotNowDisplay()
            });
        }
    });
    copilotState.pending.clear();
    if (copilotState.spawnPendingId) {
        appendCopilotMessage({
            kind: 'error',
            text: 'Bridge offline: результат запуска неизвестен.'
        });
    }
    if (copilotState.stopPendingId) {
        appendCopilotMessage({
            kind: 'error',
            text: 'Bridge offline: результат остановки неизвестен.'
        });
    }
    copilotState.spawnPendingId = null;
    copilotState.stopPendingId = null;
    updateCopilotControls();
}

function initCopilotPanel() {
    const input = document.getElementById('copilot-input');
    if (input) {
        input.addEventListener('keydown', (e) => {
            if (e.isComposing) return;
            if (e.key === 'Enter' && !e.shiftKey) {
                e.preventDefault();
                submitCopilotChat();
            }
        });
    }
}


// --- Segment Audio Playback ---

let currentAudio = null;
let currentPlayButton = null;
let currentAudioUrl = null;
let audioLoadingFor = null;  // Track which segment is loading to prevent race conditions

async function playSegmentAudio(meetingId, segmentIndex, buttonEl) {
    if (!meetingId) return;

    // If already loading for this button, ignore
    if (audioLoadingFor === buttonEl) {
        return;
    }

    // If already playing this segment, stop it
    if (currentAudio && currentPlayButton === buttonEl) {
        stopAudio();
        return;
    }

    // Stop any currently playing audio
    stopAudio();

    // Mark as loading for this button
    audioLoadingFor = buttonEl;

    // Update button to loading state
    buttonEl.classList.add('loading');
    buttonEl.disabled = true;

    try {
        const audioApiUrl = `/api/meetings/${meetingId}/audio/${segmentIndex}`;
        const response = await fetch(audioApiUrl);

        // Check if we're still supposed to be loading for this button
        if (audioLoadingFor !== buttonEl) {
            return;  // Another click happened, abort
        }

        if (!response.ok) {
            const error = await response.json().catch(() => ({}));
            throw new Error(error.detail || 'Failed to load audio');
        }

        const audioBlob = await response.blob();

        // Check again after blob load
        if (audioLoadingFor !== buttonEl) {
            return;
        }

        const audioObjectUrl = URL.createObjectURL(audioBlob);

        currentAudio = new Audio(audioObjectUrl);
        currentAudioUrl = audioObjectUrl;
        currentPlayButton = buttonEl;
        audioLoadingFor = null;

        // Update button to playing state
        buttonEl.classList.remove('loading');
        buttonEl.classList.add('playing');
        buttonEl.innerHTML = `
            <svg class="stop-icon" viewBox="0 0 24 24" width="16" height="16">
                <rect x="6" y="6" width="12" height="12" fill="currentColor"/>
            </svg>
        `;
        buttonEl.disabled = false;

        // Highlight the segment
        const segmentEl = buttonEl.closest('.transcript-segment');
        if (segmentEl) {
            segmentEl.classList.add('segment-playing');
        }

        currentAudio.onended = () => {
            stopAudio();
        };

        currentAudio.onerror = () => {
            stopAudio();
            showNotification('Audio playback failed', 'error');
        };

        await currentAudio.play();

    } catch (e) {
        audioLoadingFor = null;
        stopAudio();
        buttonEl.classList.remove('loading');
        buttonEl.disabled = false;
        showNotification(`Audio error: ${e.message}`, 'error');
    }
}

function stopAudio() {
    audioLoadingFor = null;

    if (currentAudio) {
        currentAudio.pause();
        currentAudio.onended = null;
        currentAudio.onerror = null;
        currentAudio = null;
    }

    // Revoke object URL to free memory
    if (currentAudioUrl) {
        URL.revokeObjectURL(currentAudioUrl);
        currentAudioUrl = null;
    }

    if (currentPlayButton) {
        currentPlayButton.classList.remove('playing', 'loading');
        currentPlayButton.innerHTML = `
            <svg class="play-icon" viewBox="0 0 24 24" width="16" height="16">
                <polygon points="5,3 19,12 5,21" fill="currentColor"/>
            </svg>
        `;

        // Remove playing highlight from segment
        const segmentEl = currentPlayButton.closest('.transcript-segment');
        if (segmentEl) {
            segmentEl.classList.remove('segment-playing');
        }

        currentPlayButton = null;
    }
}


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

function escapeJsString(str) {
    return String(str).replace(/\\/g, '\\\\').replace(/'/g, "\\'")
        .replace(/&/g, '&amp;').replace(/"/g, '&quot;');
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

function highlightSearch(text, term) {
    if (!term) return escapeHtml(text);
    const escaped = escapeHtml(text);
    const regex = new RegExp(`(${escapeRegex(term)})`, 'gi');
    return escaped.replace(regex, '<mark>$1</mark>');
}

// Fallback when an alias lookup misses while privacy is on. Fail closed to a
// generic label rather than leaking the real speaker name.
const PRIVACY_ALIAS_FALLBACK = 'speaker_?';

// The server owns the shared preference used by Copy/Export and Copilot MCP.
// localStorage is a display cache and a migration source for older installs.
let privacyReady = false;
let privacySaving = false;
let privacyGeneration = 0;

function syncPrivacyButtons() {
    const on = views.meeting.privacyMode;
    for (const id of ['privacy-mode-btn', 'live-privacy-btn']) {
        const btn = document.getElementById(id);
        if (btn) {
            btn.classList.toggle('active', on);
            btn.disabled = !privacyReady || privacySaving;
        }
    }
}

function applyPrivacyState(state) {
    privacyGeneration++;
    views.meeting.privacyMode = state.enabled;
    localStorage.setItem('meetingPrivacyMode', state.enabled);
    privacyReady = true;
    syncPrivacyButtons();
}

async function privacyRequest(enabled) {
    const options = enabled === undefined ? {} : {
        method: 'PUT', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ enabled }),
    };
    const response = await fetch('/api/privacy', options);
    if (!response.ok) throw new Error('Не удалось синхронизировать Privacy с Copilot MCP.');
    return response.json();
}

async function loadPrivacy() {
    if (privacySaving) return;
    const generation = ++privacyGeneration;
    try {
        let state = await privacyRequest();
        if (generation !== privacyGeneration) return;
        if (!state.configured && views.meeting.privacyMode) state = await privacyRequest(true);
        if (generation !== privacyGeneration) return;
        applyPrivacyState(state);
    } catch (error) {
        if (generation !== privacyGeneration) return;
        privacyReady = false;
        syncPrivacyButtons();
        console.error(error);
    }
}

async function togglePrivacy() {
    if (!privacyReady || privacySaving) return;
    privacySaving = true;
    privacyGeneration++;
    syncPrivacyButtons();
    try {
        applyPrivacyState(await privacyRequest(!views.meeting.privacyMode));
    } catch (error) {
        alert(error.message);
    } finally {
        privacySaving = false;
        syncPrivacyButtons();
    }
}

// Build speaker -> speaker_N aliases in order of first appearance. System event
// rows (event_type) are not speakers and are excluded from numbering.
function buildSpeakerAliasMap(segments) {
    const map = new Map();
    let n = 0;
    for (const seg of segments) {
        if (seg.event_type) continue;
        const key = seg.speaker_id || seg.speaker;
        if (key == null) continue;
        if (!map.has(key)) map.set(key, `speaker_${n++}`);
    }
    return map;
}

// Resolve a group's display name, anonymizing via the alias map when privacy is on.
function privacyName(group, alias) {
    if (!alias) return group.speaker;
    return alias.get(group.speaker_id || group.speaker) ?? PRIVACY_ALIAS_FALLBACK;
}

// Shared markdown builder for Copy and Export. When privacy is on, speaker
// names are replaced with speaker_0, speaker_1, ... consistently.
function buildMeetingMarkdown(meeting, privacy) {
    const date = new Date(meeting.meta.created_at);
    const dateStr = date.toLocaleDateString('ru-RU', {
        day: 'numeric',
        month: 'long',
        year: 'numeric'
    });

    let md = `# ${meeting.meta.title || 'Untitled Meeting'}\n\n`;
    md += `**Date:** ${dateStr}\n\n`;
    md += `---\n\n`;

    const alias = privacy ? buildSpeakerAliasMap(meeting.segments) : null;
    const grouped = meeting.groupSegments(meeting.segments);
    for (const group of grouped) {
        if (group.event_type) {
            md += `*[${group.ts_display}] ${group.text}*\n\n`;
        } else {
            md += `**${privacyName(group, alias)}** *[${group.ts_display}]*: ${group.text}\n\n`;
        }
    }
    return md;
}

// Shared markdown builder for the live view's Copy / Download. Event rows stay
// as italic system lines; speaker rows honor the same privacy alias map.
function buildLiveMarkdown(segments, title, privacy) {
    const dateStr = new Date().toLocaleDateString('ru-RU', {
        day: 'numeric',
        month: 'long',
        year: 'numeric'
    });

    let md = `# ${title}\n\n`;
    md += `**Date:** ${dateStr}\n\n`;
    md += `---\n\n`;

    const alias = privacy ? buildSpeakerAliasMap(segments) : null;
    const grouped = views.meeting.groupSegments(segments);
    for (const group of grouped) {
        if (group.event_type) {
            md += `*[${group.ts_display}] ${group.text}*\n\n`;
        } else {
            md += `**${privacyName(group, alias)}** *[${group.ts_display}]*: ${group.text}\n\n`;
        }
    }
    return md;
}

function copyToClipboard() {
    const meeting = views.meeting;
    if (!meeting.meta || !meeting.segments) return;

    const md = buildMeetingMarkdown(meeting, meeting.privacyMode);

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

function copyLiveToClipboard() {
    const segments = views.live.segments;
    if (!segments || segments.length === 0) return;

    const meeting = views.live.currentMeeting;
    const title = meeting?.title || 'Live Transcription';
    const md = buildLiveMarkdown(segments, title, views.meeting.privacyMode);

    navigator.clipboard.writeText(md).then(() => {
        const btn = document.getElementById('copy-live-btn');
        const originalText = btn.textContent;
        btn.textContent = 'Copied!';
        setTimeout(() => btn.textContent = originalText, 1500);
    }).catch(err => {
        console.error('Copy failed:', err);
    });
}

function downloadLiveMarkdown() {
    const segments = views.live.segments;
    if (!segments || segments.length === 0) return;

    const meeting = views.live.currentMeeting;
    const title = meeting?.title || 'Live Transcription';
    const md = buildLiveMarkdown(segments, title, views.meeting.privacyMode);

    const blob = new Blob([md], { type: 'text/markdown' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    const filename = meeting?.id || `live-${Date.now()}`;
    a.download = `${filename}.md`;
    a.click();
    URL.revokeObjectURL(url);
}

function exportMarkdown() {
    const meeting = views.meeting;
    if (!meeting.meta || !meeting.segments) return;

    const md = buildMeetingMarkdown(meeting, meeting.privacyMode);

    // Download
    const blob = new Blob([md], { type: 'text/markdown' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `${meeting.id}.md`;
    a.click();
    URL.revokeObjectURL(url);
}

async function generateTitle() {
    const meeting = views.meeting;
    if (!meeting.id) return;

    const btn = document.querySelector('.btn-generate-title');
    if (!btn) return;

    const originalText = btn.textContent;
    btn.textContent = 'Generating...';
    btn.disabled = true;

    try {
        const result = await api.generateTitle(meeting.id);

        // Update title in UI
        const titleEl = document.getElementById('meeting-title');
        if (titleEl) {
            titleEl.textContent = result.title;
        }

        // Update meta
        meeting.meta.title = result.title;
        meeting.meta.title_source = result.title_source;

        // Re-render meta (button will disappear)
        const metaEl = document.getElementById('meeting-meta');
        views.meeting.renderMeta(meeting.meta, metaEl);

    } catch (e) {
        btn.textContent = 'Error: ' + e.message;
        setTimeout(() => {
            btn.textContent = originalText;
            btn.disabled = false;
        }, 3000);
    }
}


async function deleteMeeting() {
    const meeting = views.meeting;
    if (!meeting.id) return;
    const title = meeting.meta?.title || 'Untitled';
    if (!window.confirm(`Удалить митинг «${title}»? Действие необратимо.`)) return;

    const btn = document.getElementById('delete-meeting-btn');
    if (btn) { btn.textContent = 'Deleting...'; btn.disabled = true; }
    try {
        await api.deleteMeeting(meeting.id);
        showNotification('Митинг удалён', 'success');
        router.navigate('/');
    } catch (e) {
        showNotification('Ошибка удаления: ' + e.message, 'error');
        if (btn) { btn.textContent = 'Delete'; btn.disabled = false; }
    }
}


function showNotification(message, type = 'success') {
    // Simple notification - could be enhanced with a toast library
    const notification = document.createElement('div');
    notification.className = `notification notification-${type}`;
    notification.textContent = message;
    notification.style.cssText = `
        position: fixed;
        top: 20px;
        right: 20px;
        padding: 12px 20px;
        border-radius: 8px;
        background: ${type === 'error' ? '#ef4444' : '#22c55e'};
        color: white;
        font-weight: 500;
        z-index: 1200;
        animation: slideIn 0.3s ease;
    `;
    document.body.appendChild(notification);

    setTimeout(() => {
        notification.style.animation = 'slideOut 0.3s ease';
        setTimeout(() => notification.remove(), 300);
    }, 3000);
}


// --- AI Analysis ---

let aiResponses = {};  // Store responses for each provider
let aiProviderNames = {};  // Store provider display names
let activeAiTab = null;
let aiEventSource = null;
let aiModalSession = null;
let aiContentRequest = 0;
let incrementPollTimer = null;
let _incrementState = null;  // Stored increment modal state, including participant selection
const SYSTEM_PARTICIPANT_ID = '__system__';

async function analyzeIncrement() {
    const meeting = views.meeting;
    if (!meeting.id) return;
    closeAiModal();
    aiModalSession = {
        meetingId: meeting.id, mode: 'saved', historyRequest: 0,
        historyStatus: 'loading', runStatus: 'idle', providerStates: {}, runError: ''
    };
    aiResponses = {};
    aiProviderNames = {};
    activeAiTab = null;
    document.getElementById('ai-modal').style.display = 'flex';
    renderNewAnalysis();
    switchAiMode('saved');
    await loadAiHistory(true);
}

async function loadAiHistory(selectEmpty = false) {
    const session = aiModalSession;
    if (!session) return;
    const request = ++session.historyRequest;
    session.historyStatus = 'loading';
    const saved = document.getElementById('ai-saved-view');
    saved.querySelector('.modal-tabs').innerHTML = '';
    saved.querySelector('.modal-body').innerHTML = '<div class="loading">Загрузка сохранённых результатов...</div>';
    saved.querySelector('.modal-footer').innerHTML = '<button class="btn" onclick="closeAiModal()">Close</button>';
    try {
        const [prompts, increments] = await Promise.all([
            api.getPrompts(), api.getIncrements(session.meetingId)
        ]);
        if (aiModalSession !== session || session.historyRequest !== request) return;
        session.historyStatus = 'ready';
        if (Object.keys(increments).length) {
            showStoredIncrements(session.meetingId, increments, prompts);
        } else {
            saved.querySelector('.modal-body').innerHTML = '<div class="empty">Сохранённых результатов пока нет.</div>';
            if (selectEmpty && session.mode === 'saved') switchAiMode('new');
        }
    } catch (e) {
        if (aiModalSession !== session || session.historyRequest !== request) return;
        session.historyStatus = 'error';
        saved.querySelector('.modal-body').innerHTML =
            '<div class="error">Не удалось загрузить сохранённые результаты: ' + escapeHtml(e.message) + '</div>' +
            '<button class="btn" onclick="loadAiHistory()">Повторить загрузку</button>';
    }
}

function switchAiMode(mode) {
    if (!aiModalSession || !['saved', 'new'].includes(mode)) return;
    aiModalSession.mode = mode;
    aiContentRequest++;
    if (incrementPollTimer) {
        clearInterval(incrementPollTimer);
        incrementPollTimer = null;
    }
    document.querySelectorAll('#ai-modal [data-ai-mode]').forEach(function(btn) {
        btn.classList.toggle('active', btn.dataset.aiMode === mode);
        btn.setAttribute('aria-pressed', String(btn.dataset.aiMode === mode));
    });
    document.getElementById('ai-saved-view').hidden = mode !== 'saved';
    document.getElementById('ai-new-view').hidden = mode !== 'new';
    updateAiRunNotice();
    if (mode === 'saved' && _incrementState) {
        if (_incrementState.needsRefresh) {
            _incrementState.needsRefresh = false;
            switchPromptTab(_incrementState.meetingId, _incrementState.currentPromptId);
        } else if (_incrementState.currentVersion == null) {
            renderMissingIncrement();
        } else if (!_incrementState.content) {
            loadIncrementContent(_incrementState.meetingId, _incrementState.currentPromptId, _incrementState.currentVersion);
        }
    }
}

function updateAiRunNotice() {
    const notice = document.getElementById('ai-run-notice');
    notice.hidden = !aiModalSession || aiModalSession.mode !== 'saved' || aiModalSession.runStatus === 'idle';
    notice.textContent = aiModalSession?.runStatus === 'running'
        ? 'Новый анализ продолжается. Результаты — во вкладке «Новый анализ».'
        : 'Новый анализ завершён. Результаты — во вкладке «Новый анализ».';
}

function renderNewAnalysis() {
    const session = aiModalSession;
    if (!session) return;
    const providers = Object.keys(aiProviderNames);
    document.getElementById('ai-provider-tabs').innerHTML = providers.map(function(provider) {
        return '<button class="tab-btn' + (provider === activeAiTab ? ' active' : '') +
            '" data-provider="' + escapeHtml(provider) + '" onclick="switchAiTab(\'' + escapeJsString(provider) + '\')">' +
            escapeHtml(aiProviderNames[provider]) + '</button>';
    }).join('');
    const content = document.getElementById('ai-new-content');
    content.innerHTML = (session.runError ? '<div class="error">' + escapeHtml(session.runError) + '</div>' : '') +
        (providers.length ? providers.map(function(provider) {
            const state = session.providerStates[provider];
            const answer = aiResponses[provider];
            return '<div id="ai-response-' + escapeHtml(provider) + '" class="ai-tab-content' +
                (provider === activeAiTab ? ' active' : '') + '">' +
                '<div class="increment-result-meta"><span class="increment-model-badge">Model: ' +
                escapeHtml(aiProviderNames[provider]) + '</span></div>' +
                (answer ? '<div class="ai-answer">' + (state.status === 'running' ? '<span class="streaming-indicator">●</span> ' : '') + renderMarkdown(answer) + '</div>' : '') +
                (state.error ? '<div class="error">' + escapeHtml(state.error) + '</div>' :
                    !answer && state.status === 'running' ? '<div class="loading">Анализ...</div>' : '') + '</div>';
        }).join('') : session.runStatus === 'running' ? '<div class="loading">Подключение...</div>' :
            session.runStatus === 'idle' ? '<div class="empty">Нажмите «Запустить анализ», чтобы начать.</div>' : '');
    const button = document.getElementById('ai-run-button');
    button.disabled = session.runStatus === 'running';
    button.textContent = session.runStatus === 'running' ? 'Анализ выполняется...' :
        session.runStatus === 'idle' ? 'Запустить анализ' : 'Запустить заново';
    updateAiRunNotice();
}

function generateIncrementSSE() {
    const session = aiModalSession;
    if (!session || session.runStatus === 'running') return;
    aiResponses = {};
    aiProviderNames = {};
    activeAiTab = null;
    session.providerStates = {};
    session.runStatus = 'running';
    session.runError = '';
    renderNewAnalysis();
    const source = new EventSource(`/api/meetings/${encodeURIComponent(session.meetingId)}/analyze-stream?question=increment`);
    aiEventSource = source;
    source.onmessage = (event) => {
        if (aiModalSession !== session || aiEventSource !== source) return;
        try {
            handleAiStreamEvent(JSON.parse(event.data));
        } catch (e) {
            console.error('Failed to parse SSE event:', e);
        }
    };
    source.onerror = () => {
        if (aiModalSession !== session || aiEventSource !== source) return;
        session.runError = 'Соединение прервано. Полученные результаты показаны ниже; анализ можно запустить заново.';
        finishAiStream();
    };
}

function finishAiStream() {
    if (aiEventSource) aiEventSource.close();
    aiEventSource = null;
    aiModalSession.runStatus = 'complete';
    Object.values(aiModalSession.providerStates).forEach(function(state) { state.status = 'done'; });
    renderNewAnalysis();
}

function showStoredIncrements(meetingId, incrementsByPrompt, prompts) {
    if (incrementPollTimer) {
        clearInterval(incrementPollTimer);
        incrementPollTimer = null;
    }
    if (!aiModalSession || aiModalSession.meetingId !== meetingId) return;
    const previous = _incrementState;

    // Build prompt name lookup
    const promptNames = {};
    (prompts || []).forEach(function(p) { promptNames[p.id] = p.name; });

    const promptIds = Object.keys(incrementsByPrompt);
    if (promptIds.length === 0) return;

    const meetingParticipants = Object.entries(views.meeting.meta?.participants || {});
    const localNames = meetingParticipants
        .filter(function(entry) {
            return entry[0] !== SYSTEM_PARTICIPANT_ID && entry[1].is_local;
        })
        .map(function(entry) { return entry[1].name || entry[0]; });
    const seenParticipantNames = new Set(localNames);
    const otherParticipants = [];
    meetingParticipants.forEach(function(entry) {
        if (entry[0] === SYSTEM_PARTICIPANT_ID || entry[1].is_local) return;
        const name = entry[1].name || entry[0];
        if (seenParticipantNames.has(name)) return;
        seenParticipantNames.add(name);
        otherParticipants.push({ id: entry[0], name: name });
    });
    const incrementParticipants = [{
        id: null,
        name: localNames.length > 0 ? localNames.join(', ') : 'Me'
    }].concat(otherParticipants);

    const modal = document.getElementById('ai-saved-view');

    // Level 1: prompt tabs
    const promptTabsHtml = promptIds.map(function(pid, i) {
        var name = promptNames[pid] || pid;
        var isActive = i === 0 ? ' active' : '';
        return '<button class="tab-btn' + isActive + '" data-prompt-id="' + escapeHtml(pid) +
            '" onclick="switchPromptTab(\'' + escapeJsString(meetingId) + '\', \'' + escapeJsString(pid) + '\')">' +
            escapeHtml(name) + '</button>';
    }).join('');
    modal.querySelector('.modal-tabs').innerHTML = promptTabsHtml;

    // Body: version sub-tabs + content area
    modal.querySelector('.modal-body').innerHTML =
        '<div class="increment-participant-toolbar" id="increment-participant-toolbar">' +
            '<label for="increment-participant-select">Participant</label>' +
            '<select id="increment-participant-select" onchange="switchIncrementParticipant(this.value)"></select>' +
        '</div>' +
        '<div class="increment-version-tabs" id="version-tabs"></div>' +
        '<div id="increment-content" class="ai-tab-content active"><div class="loading">Loading...</div></div>';

    modal.querySelector('.modal-footer').innerHTML =
        '<button class="btn" onclick="copyAiResponse()">Copy</button>' +
        '<button class="btn" onclick="regeneratePrompt()">Regenerate</button>' +
        '<button class="btn" onclick="closeAiModal()">Close</button>';

    // Store state
    _incrementState = {
        meetingId: meetingId,
        incrementsByPrompt: incrementsByPrompt,
        promptNames: promptNames,
        currentPromptId: previous && promptIds.includes(previous.currentPromptId) ? previous.currentPromptId : promptIds[0],
        currentVersion: null,
        currentParticipantId: previous ? previous.currentParticipantId : null,
        participants: incrementParticipants,
        pendingGenerations: previous ? previous.pendingGenerations : new Set(),
        generationErrors: previous ? previous.generationErrors : {},
        content: '',
        selectionRevision: 0
    };

    const participantSelect = document.getElementById('increment-participant-select');
    incrementParticipants.forEach(function(participant) {
        const option = document.createElement('option');
        option.value = participant.id || '';
        option.textContent = participant.name;
        participantSelect.appendChild(option);
    });

    participantSelect.value = _incrementState.currentParticipantId || '';
    switchPromptTab(meetingId, _incrementState.currentPromptId);
}

function switchPromptTab(meetingId, promptId) {
    if (!_incrementState || _incrementState.meetingId !== meetingId) return;
    _incrementState.needsRefresh = false;
    aiContentRequest++;
    _incrementState.selectionRevision++;
    _incrementState.content = '';
    if (incrementPollTimer) {
        clearInterval(incrementPollTimer);
        incrementPollTimer = null;
    }
    // Update Level 1 prompt tab active state
    document.querySelectorAll('#ai-saved-view .modal-tabs .tab-btn').forEach(function(btn) {
        btn.classList.toggle('active', btn.dataset.promptId === promptId);
    });

    if (_incrementState) {
        _incrementState.currentPromptId = promptId;
    }

    var metas = (_incrementState && _incrementState.incrementsByPrompt[promptId]) || [];
    var isParticipantIncrement = promptId === 'my-increment';
    var participantToolbar = document.getElementById('increment-participant-toolbar');
    if (participantToolbar) {
        participantToolbar.style.display = isParticipantIncrement ? '' : 'none';
    }
    if (isParticipantIncrement && _incrementState) {
        metas = metas.filter(function(meta) {
            return (meta.participant_id || null) === _incrementState.currentParticipantId;
        });
    }
    var versionTabsContainer = document.getElementById('version-tabs');

    if (metas.length === 0) {
        if (versionTabsContainer) versionTabsContainer.innerHTML = '';
        _incrementState.currentVersion = null;
        renderMissingIncrement();
        return;
    }

    // Build Level 2 version tabs
    var latestVersion = metas[metas.length - 1].version;
    if (_incrementState) {
        _incrementState.currentVersion = latestVersion;
    }

    if (versionTabsContainer) {
        versionTabsContainer.innerHTML = metas.map(function(inc) {
            var date = new Date(inc.generated_at);
            var timeStr = date.toLocaleTimeString('ru-RU', { hour: '2-digit', minute: '2-digit' });
            var isActive = inc.version === latestVersion ? ' active' : '';
            var statusIcon = inc.status === 'failed' ? ' !' : inc.status === 'generating' ? ' ...' : '';
            return '<button class="version-tab-btn' + isActive + '" data-version="' + inc.version +
                '" onclick="switchIncrementVersion(\'' + escapeJsString(meetingId) + '\', \'' + escapeJsString(promptId) + '\', ' + inc.version +
                ')">v' + inc.version + ' <small style="opacity:.6">' + timeStr + '</small>' +
                statusIcon + '</button>';
        }).join('');
    }

    loadIncrementContent(meetingId, promptId, latestVersion);
}

function renderMissingIncrement() {
    const state = _incrementState;
    if (!state) return;
    const key = state.currentParticipantId || '__local__';
    const isParticipantIncrement = state.currentPromptId === 'my-increment';
    const pending = isParticipantIncrement && state.pendingGenerations.has(key);
    const error = isParticipantIncrement && state.generationErrors[key];
    document.getElementById('increment-content').innerHTML = pending
        ? '<div class="loading">Generating increment...</div>'
        : (error ? '<div class="error">Generation failed: ' + escapeHtml(error) + '</div>' :
            '<div class="empty">Сохранённых версий пока нет.</div>') +
            (isParticipantIncrement ? '<button class="btn" onclick="generateParticipantIncrement()">Создать сохранённый результат</button>' : '');
}

function switchIncrementParticipant(participantId) {
    if (!_incrementState) return;

    _incrementState.currentParticipantId = participantId || null;
    switchPromptTab(_incrementState.meetingId, _incrementState.currentPromptId);
}

async function generateParticipantIncrement() {
    if (aiModalSession?.mode !== 'saved' || !_incrementState || _incrementState.currentPromptId !== 'my-increment') return;

    const state = _incrementState;
    const meetingId = state.meetingId;
    const participantId = state.currentParticipantId;
    const pendingKey = participantId || '__local__';
    const container = document.getElementById('increment-content');

    if (state.pendingGenerations.has(pendingKey)) {
        if (container) container.innerHTML = '<div class="loading">Generating increment...</div>';
        return;
    }

    state.pendingGenerations.add(pendingKey);
    delete state.generationErrors[pendingKey];
    if (container) container.innerHTML = '<div class="loading">Generating increment...</div>';

    try {
        const params = new URLSearchParams();
        if (participantId) params.set('participant_id', participantId);
        const query = params.toString();
        const res = await fetch('/api/meetings/' + encodeURIComponent(meetingId) +
            '/increments/my-increment/generate' + (query ? '?' + query : ''), { method: 'POST' });
        if (!res.ok) {
            const err = await res.json().catch(function() { return {}; });
            throw new Error(err.detail || 'Generation failed');
        }
        const generation = await res.json();
        if (generation.status === 'skipped') {
            throw new Error(generation.message || 'No new segments to process');
        }

        const increments = await api.getIncrements(meetingId);
        if (_incrementState !== state) return;
        state.incrementsByPrompt = increments;
        state.needsRefresh = true;

        if (aiModalSession?.mode === 'saved' && state.currentPromptId === 'my-increment' && state.currentParticipantId === participantId) {
            switchPromptTab(meetingId, 'my-increment');
        }
    } catch (e) {
        if (_incrementState === state) state.generationErrors[pendingKey] = e.message;
    } finally {
        state.pendingGenerations.delete(pendingKey);
        if (_incrementState === state && aiModalSession?.mode === 'saved' &&
            state.currentPromptId === 'my-increment' && state.currentParticipantId === participantId &&
            state.currentVersion == null) renderMissingIncrement();
    }
}

function switchIncrementVersion(meetingId, promptId, version) {
    if (!_incrementState) return;
    _incrementState.selectionRevision++;
    if (incrementPollTimer) {
        clearInterval(incrementPollTimer);
        incrementPollTimer = null;
    }

    if (_incrementState) {
        _incrementState.currentVersion = version;
    }

    document.querySelectorAll('#version-tabs .version-tab-btn').forEach(function(btn) {
        btn.classList.toggle('active', parseInt(btn.dataset.version) === version);
    });

    loadIncrementContent(meetingId, promptId, version);
}

function stripYamlFrontmatter(text) {
    if (!text || !text.startsWith('---')) return text;
    const match = text.substring(3).match(/\n---\s*\n/);
    if (!match) return text;
    return text.substring(3 + match.index + match[0].length).trim();
}

async function loadIncrementContent(meetingId, promptId, version) {
    if (!aiModalSession || aiModalSession.mode !== 'saved') return;
    const session = aiModalSession;
    const state = _incrementState;
    const request = ++aiContentRequest;
    const isCurrent = () => aiModalSession === session && session.mode === 'saved' &&
        _incrementState === state && request === aiContentRequest;
    if (incrementPollTimer) {
        clearInterval(incrementPollTimer);
        incrementPollTimer = null;
    }

    const container = document.getElementById('increment-content');
    if (!container) return;

    state.content = '';
    container.innerHTML = '<div class="loading">Loading...</div>';

    try {
        const result = await api.getIncrement(meetingId, promptId, version);
        if (!isCurrent()) return;
        if (!result) {
            container.innerHTML = '<div class="error">Version not found</div>';
            return;
        }

        const { meta, content } = result;

        if (meta.status === 'failed') {
            const errorMsg = meta.error || 'Generation failed';
            container.innerHTML = '<div class="error">Generation failed: ' + escapeHtml(errorMsg) + '</div>';
            state.content = '';
            return;
        }

        if (meta.status === 'generating') {
            container.innerHTML = '<div class="loading">Generation in progress...</div>';
            // The placeholder resolves to completed/failed once the backend
            // finishes. Poll until then — otherwise this view stays on this
            // message until a manual page refresh. The clearInterval at the
            // top of this function makes each re-entry cancel the previous
            // tick, so no timers stack. (incrementPollTimer was declared and
            // cleared everywhere but never actually started.)
            incrementPollTimer = setInterval(function() {
                loadIncrementContent(meetingId, promptId, version);
            }, 2000);
            return;
        }

        // status === 'completed'
        const strippedContent = stripYamlFrontmatter(content);
        const rendered = renderMarkdown(strippedContent);
        const durationStr = meta.duration_seconds != null ? meta.duration_seconds.toFixed(1) + 's' : '';
        const promptName = (_incrementState && _incrementState.promptNames[promptId]) || promptId;
        const participantStr = meta.participant_name ? ' · ' + meta.participant_name : '';
        const infoStr = promptName + participantStr + ' · v' + meta.version + ' · ' + meta.segment_count + ' segments' +
            (durationStr ? ' · ' + durationStr : '');
        const providerName = meta.provider === 'codex' ? 'Codex' : meta.provider === 'claude' ? 'Claude' : (meta.provider || '');
        const modelStr = [providerName, meta.model || 'Model unavailable', meta.reasoning_effort || meta.effort]
            .filter(Boolean).join(' · ');

        container.innerHTML =
            '<div class="increment-result-meta">' +
                '<span class="increment-model-badge">Model: ' + escapeHtml(modelStr) + '</span>' +
                '<span class="increment-result-details">' + escapeHtml(infoStr) + '</span>' +
            '</div>' +
            '<div class="ai-answer">' + rendered + '</div>';

        state.content = strippedContent;
    } catch (e) {
        if (!isCurrent()) return;
        container.innerHTML = '<div class="error">Failed to load: ' + escapeHtml(e.message) + '</div>';
    }
}

async function regeneratePrompt() {
    const session = aiModalSession;
    const state = _incrementState;
    if (!session || session.mode !== 'saved' || !state) return;
    aiContentRequest++;
    const selectionRevision = state.selectionRevision;
    const isCurrent = () => aiModalSession === session && _incrementState === state &&
        session.mode === 'saved' && selectionRevision === state.selectionRevision;
    if (incrementPollTimer) {
        clearInterval(incrementPollTimer);
        incrementPollTimer = null;
    }
    var meetingId = _incrementState ? _incrementState.meetingId : (views.meeting && views.meeting.id);
    var promptId = _incrementState ? _incrementState.currentPromptId : null;
    var participantId = _incrementState && promptId === 'my-increment'
        ? _incrementState.currentParticipantId
        : null;

    if (!meetingId || !promptId) return;

    // Show generating state
    state.content = '';
    var container = document.getElementById('increment-content');
    if (container) container.innerHTML = '<div class="loading">Generating...</div>';

    try {
        // force=true: regenerate over the full transcript. Without it a finished
        // meeting (last version already covers every segment) skips with
        // "No new segments to process".
        var params = new URLSearchParams({ force: 'true' });
        if (participantId) params.set('participant_id', participantId);
        var res = await fetch('/api/meetings/' + encodeURIComponent(meetingId) +
            '/increments/' + encodeURIComponent(promptId) + '/generate?' + params.toString(), { method: 'POST' });
        if (!res.ok) {
            var err = await res.json().catch(function() { return {}; });
            throw new Error(err.detail || 'Generation failed');
        }
        var result = await res.json();
        if (aiModalSession !== session || _incrementState !== state) return;
        if (result.status === 'skipped') {
            if (isCurrent() && container) container.innerHTML = '<div class="empty">' + escapeHtml(result.message || 'No new segments') + '</div>';
            return;
        }
        // Refresh the stored increments view
        var [prompts, increments] = await Promise.all([
            api.getPrompts(),
            api.getIncrements(meetingId)
        ]);
        if (aiModalSession !== session || _incrementState !== state) return;
        state.incrementsByPrompt = increments;
        state.needsRefresh = selectionRevision === state.selectionRevision;
        if (isCurrent()) showStoredIncrements(meetingId, increments, prompts);
    } catch (e) {
        if (isCurrent() && container) container.innerHTML = '<div class="error">Regeneration failed: ' + escapeHtml(e.message) + '</div>';
    }
}

function handleAiStreamEvent(data) {
    if (!aiModalSession) return;
    const { type, provider, name, text, message } = data;
    const states = aiModalSession.providerStates;
    if (type === 'start' && provider) {
        aiProviderNames[provider] = name || provider;
        states[provider] = { status: 'running', error: '' };
        if (!activeAiTab) activeAiTab = provider;
    } else if ((type === 'chunk' || type === 'done') && states[provider]) {
        aiResponses[provider] = text;
        states[provider].status = type === 'done' ? 'done' : 'running';
    } else if (type === 'error') {
        if (states[provider]) {
            states[provider].error = message || 'Анализ завершился с ошибкой';
            states[provider].status = 'done';
        } else {
            aiModalSession.runError = message || 'Не удалось запустить анализ';
            finishAiStream();
            return;
        }
    } else if (type === 'complete') {
        if (!Object.keys(states).length) aiModalSession.runError = 'Нет доступных провайдеров для анализа.';
        finishAiStream();
        return;
    }
    renderNewAnalysis();
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
    if (!aiModalSession || !aiProviderNames[provider]) return;
    activeAiTab = provider;
    document.querySelectorAll('#ai-provider-tabs .tab-btn').forEach(function(btn) {
        btn.classList.toggle('active', btn.dataset.provider === provider);
    });
    document.querySelectorAll('#ai-new-content .ai-tab-content').forEach(function(content) {
        content.classList.toggle('active', content.id === `ai-response-${provider}`);
    });
}

function closeAiModal() {
    if (aiEventSource) {
        aiEventSource.close();
        aiEventSource = null;
    }
    if (incrementPollTimer) {
        clearInterval(incrementPollTimer);
        incrementPollTimer = null;
    }
    aiModalSession = null;
    _incrementState = null;
    aiContentRequest++;
    document.getElementById('ai-modal').style.display = 'none';
}

function copyAiResponse() {
    if (!aiModalSession) return;
    const text = aiModalSession.mode === 'saved' ? _incrementState?.content : aiResponses[activeAiTab];
    if (text) {
        const pane = document.getElementById(aiModalSession.mode === 'saved' ? 'ai-saved-view' : 'ai-new-view');
        const btn = pane.querySelector('.modal-footer .btn[onclick="copyAiResponse()"]');
        navigator.clipboard.writeText(text).then(() => {
            const originalText = btn.textContent;
            btn.textContent = 'Copied!';
            setTimeout(() => btn.textContent = originalText, 1500);
        });
    }
}


function formatTime(seconds) {
    const m = Math.floor(seconds / 60);
    const s = Math.floor(seconds % 60);
    return `${m}:${s.toString().padStart(2, '0')}`;
}

// --- Init ---

document.addEventListener('DOMContentLoaded', () => {
    router.init();
    initCopilotPanel();
    loadPrivacy();
});
document.addEventListener('visibilitychange', () => {
    if (!document.hidden) loadPrivacy();
});
