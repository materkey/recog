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

    async getLiveAnchor(payload) {
        const res = await fetch('/api/live/anchor', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(payload)
        });
        if (!res.ok) {
            const error = await res.json().catch(() => ({}));
            throw new Error(error.detail || 'Failed to fetch context');
        }
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
        return res.json();
    },

    async getPrompts() {
        const res = await fetch('/api/prompts');
        if (!res.ok) return [];
        return res.json();
    },

    async getIncrements(meetingId) {
        const res = await fetch(`/api/meetings/${meetingId}/increments`);
        if (!res.ok) return {};
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
        '/meeting/:id': 'meeting',
        '/speakers': 'speakers'
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
        } else if (hash === '/speakers') {
            this.showView('speakers');
            views.speakers.load();
        }
    },

    showView(name) {
        // Disconnect live if leaving
        if (name !== 'live') {
            views.live.disconnect();
            closeAnchor();
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
        _listenersInitialized: false,

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
        _searchListenerInitialized: false,
        enrollMode: false,
        selectedSegments: [],  // Array of segment indices for enrollment

        async load(id) {
            // Reset enrollment state
            this.enrollMode = false;
            this.selectedSegments = [];
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

            // Audio tracks and diarization
            const hasMicAudio = meta.audio_mic_path && !meta.audio_deleted_at;
            const hasSystemAudio = meta.audio_system_path && !meta.audio_deleted_at;
            const hasDiarizationMic = meta.diarization_mic;
            const hasDiarizationSystem = meta.diarization_system;
            const hasBackup = meta.transcript_backup_path;

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
                                    ${hasDiarizationMic
                                        ? `<span class="track-status diarized">✓ ${meta.diarization_mic.speakers_found} speakers</span>`
                                        : `<button class="btn btn-diarize" onclick="diarizeTrack('mic')">Diarize</button>`
                                    }
                                </div>
                            ` : ''}
                            ${hasSystemAudio ? `
                                <div class="audio-track">
                                    <span class="track-icon">🔊</span>
                                    <span class="track-name">System</span>
                                    ${hasDiarizationSystem
                                        ? `<span class="track-status diarized">✓ ${meta.diarization_system.speakers_found} speakers</span>`
                                        : `<button class="btn btn-diarize" onclick="diarizeTrack('system')">Diarize</button>`
                                    }
                                </div>
                            ` : ''}
                        </div>
                        ${(hasDiarizationMic || hasDiarizationSystem) && hasBackup ? `
                            <button class="btn btn-undo-diarize" onclick="undoDiarization()">Undo All Diarization</button>
                        ` : ''}
                    </div>
                ` : ''}
            `;
        },

        renderTranscript(segments, container, searchTerm = '') {
            if (segments.length === 0) {
                container.innerHTML = '<div class="empty">No transcript segments</div>';
                return;
            }

            // Group by speaker, but preserve segment indices for enrollment
            const grouped = this.groupSegments(segments);

            const enrollModeClass = this.enrollMode ? 'enroll-mode' : '';
            container.className = `transcript ${enrollModeClass}`;

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

                // For enrollment: check if segment has audio data
                const hasAudio = group.segmentIndices && group.segmentIndices.some(idx => {
                    const seg = segments[idx];
                    return seg && seg.audio_offset_ms !== undefined;
                });
                const segmentIndices = group.segmentIndices ? group.segmentIndices.join(',') : '';
                const isSelected = group.segmentIndices && group.segmentIndices.some(idx =>
                    this.selectedSegments.includes(idx)
                );

                // Checkbox for enrollment mode
                const checkbox = this.enrollMode ? `
                    <label class="segment-checkbox ${hasAudio ? '' : 'disabled'}" title="${hasAudio ? 'Select for enrollment' : 'No audio data'}">
                        <input type="checkbox"
                            ${hasAudio ? '' : 'disabled'}
                            ${isSelected ? 'checked' : ''}
                            onchange="toggleSegmentSelection(${segmentIndices})"
                        >
                    </label>
                ` : '';

                // Play button for segments with audio (only first segment in group)
                const firstSegmentIdx = group.segmentIndices ? group.segmentIndices[0] : null;
                const playButton = hasAudio && !this.enrollMode && firstSegmentIdx !== null ? `
                    <button class="btn-play-segment" onclick="playSegmentAudio(${firstSegmentIdx}, this)" title="Play audio">
                        <svg class="play-icon" viewBox="0 0 24 24" width="16" height="16">
                            <polygon points="5,3 19,12 5,21" fill="currentColor"/>
                        </svg>
                    </button>
                ` : '';

                return `
                    <div class="transcript-segment ${speakerClass} ${isSelected ? 'segment-selected' : ''}"
                         data-segment-indices="${segmentIndices}">
                        ${checkbox}
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

            // Render enrollment action bar if segments selected
            this.renderEnrollmentBar();
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

        renderEnrollmentBar() {
            // Remove existing bar if any
            const existingBar = document.getElementById('enrollment-bar');
            if (existingBar) existingBar.remove();

            if (!this.enrollMode) return;

            const container = document.getElementById('view-meeting');
            const bar = document.createElement('div');
            bar.id = 'enrollment-bar';
            bar.className = 'enrollment-bar';

            const count = this.selectedSegments.length;
            bar.innerHTML = `
                <span class="enrollment-count">${count} segment(s) selected</span>
                <div class="enrollment-actions">
                    <button class="btn" onclick="clearEnrollSelection()">Clear</button>
                    <button class="btn btn-primary" onclick="openEnrollModal()" ${count === 0 ? 'disabled' : ''}>
                        Add as Speaker
                    </button>
                </div>
            `;

            container.appendChild(bar);
        },

        toggleEnrollMode() {
            this.enrollMode = !this.enrollMode;
            this.selectedSegments = [];

            // Update button
            const btn = document.getElementById('enroll-mode-btn');
            if (btn) {
                btn.textContent = this.enrollMode ? 'Cancel Selection' : 'Select for Enrollment';
                btn.classList.toggle('active', this.enrollMode);
            }

            // Re-render transcript
            const transcriptEl = document.getElementById('transcript');
            this.renderTranscript(this.segments, transcriptEl);
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
        followMode: localStorage.getItem('liveFollowMode') !== 'false',

        connect() {
            this.updateFollowButton();
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
                    lastAnchorPayload = null;
                    this.renderMeetingInfo(msg.meeting, infoEl);
                    this.renderTranscript(transcriptEl);
                    break;

                case 'meeting_change':
                    this.currentMeeting = msg.meeting;
                    this.segments = [];
                    lastAnchorPayload = null;
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
                div.dataset.tsDisplay = seg.ts_display || '';
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
    },

    // --- Speakers View ---
    speakers: {
        speakers: [],

        async load() {
            const container = document.getElementById('speakers-list');
            const countEl = document.getElementById('speakers-count');
            container.innerHTML = '<div class="loading">Loading speakers...</div>';

            try {
                const res = await fetch('/api/speakers');
                if (!res.ok) throw new Error('Failed to fetch speakers');
                this.speakers = await res.json();
                countEl.textContent = `${this.speakers.length} speakers`;
                this.render();
            } catch (e) {
                container.innerHTML = `<div class="error">Error: ${e.message}</div>`;
            }
        },

        render() {
            const container = document.getElementById('speakers-list');

            if (this.speakers.length === 0) {
                container.innerHTML = '<div class="empty-state">No speakers in database. Add speakers from meeting transcripts.</div>';
                return;
            }

            container.innerHTML = this.speakers.map(speaker => `
                <div class="speaker-card" data-speaker-id="${escapeHtml(speaker.id)}">
                    <div class="speaker-header">
                        <div class="speaker-info">
                            <span class="speaker-name">${escapeHtml(speaker.display_name)}</span>
                            <span class="speaker-id">${escapeHtml(speaker.id)}</span>
                        </div>
                        <div class="speaker-actions">
                            <button class="btn btn-small" onclick="renameSpeaker('${escapeHtml(speaker.id)}', '${escapeHtml(speaker.display_name)}')">Rename</button>
                            <button class="btn btn-small btn-danger" onclick="deleteSpeaker('${escapeHtml(speaker.id)}', '${escapeHtml(speaker.display_name)}')">Delete</button>
                        </div>
                    </div>
                    <div class="speaker-embeddings">
                        <span class="embedding-count">${speaker.embedding_count} embedding(s)</span>
                        <button class="btn btn-small btn-toggle" onclick="toggleEmbeddings('${escapeHtml(speaker.id)}')">Show Details</button>
                    </div>
                    <div class="embeddings-list" id="embeddings-${escapeHtml(speaker.id)}" style="display: none;">
                        ${speaker.embeddings.map((emb, idx) => `
                            <div class="embedding-item">
                                <div class="embedding-info">
                                    <span class="embedding-source">${escapeHtml(emb.source_file)}</span>
                                    <span class="embedding-time">${formatTime(emb.start_time)} - ${formatTime(emb.end_time)}</span>
                                    <span class="embedding-transcript">${escapeHtml(emb.transcript)}</span>
                                </div>
                                <button class="btn btn-small btn-danger" onclick="deleteEmbedding('${escapeHtml(speaker.id)}', ${idx})">Remove</button>
                            </div>
                        `).join('')}
                    </div>
                </div>
            `).join('');
        }
    }
};

function toggleFollowMode() {
    views.live.toggleFollow();
}


// --- Segment Audio Playback ---

let currentAudio = null;
let currentPlayButton = null;
let currentAudioUrl = null;
let audioLoadingFor = null;  // Track which segment is loading to prevent race conditions

async function playSegmentAudio(segmentIndex, buttonEl) {
    const meeting = views.meeting;
    if (!meeting.id) return;

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
        const audioApiUrl = `/api/meetings/${meeting.id}/audio/${segmentIndex}`;
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

function copyLiveToClipboard() {
    const segments = views.live.segments;
    if (!segments || segments.length === 0) return;

    const meeting = views.live.currentMeeting;
    const title = meeting?.title || 'Live Transcription';
    const dateStr = new Date().toLocaleDateString('ru-RU', {
        day: 'numeric',
        month: 'long',
        year: 'numeric'
    });

    let md = `# ${title}\n\n`;
    md += `**Date:** ${dateStr}\n\n`;
    md += `---\n\n`;

    const grouped = views.meeting.groupSegments(segments);
    for (const group of grouped) {
        if (group.event_type) {
            md += `*[${group.ts_display}] ${group.text}*\n\n`;
        } else {
            md += `**${group.speaker}** *[${group.ts_display}]*: ${group.text}\n\n`;
        }
    }

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
    const dateStr = new Date().toLocaleDateString('ru-RU', {
        day: 'numeric',
        month: 'long',
        year: 'numeric'
    });

    let md = `# ${title}\n\n`;
    md += `**Date:** ${dateStr}\n\n`;
    md += `---\n\n`;

    const grouped = views.meeting.groupSegments(segments);
    for (const group of grouped) {
        if (group.event_type) {
            md += `*[${group.ts_display}] ${group.text}*\n\n`;
        } else {
            md += `**${group.speaker}** *[${group.ts_display}]*: ${group.text}\n\n`;
        }
    }

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


// --- Diarization ---

async function diarizeTrack(track) {
    const meeting = views.meeting;
    if (!meeting.id) return;

    const btn = document.querySelector(`.audio-track button[onclick="diarizeTrack('${track}')"]`);
    if (!btn) return;

    const originalText = btn.textContent;
    btn.textContent = 'Diarizing...';
    btn.disabled = true;

    try {
        const response = await fetch(`/api/meetings/${meeting.id}/diarize`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ track })
        });

        if (!response.ok) {
            const error = await response.json();
            throw new Error(error.detail || 'Diarization failed');
        }

        const result = await response.json();

        // Reload meeting to show updated transcript
        await meeting.load(meeting.id);

        // Show success briefly
        showNotification(`Diarization complete: ${result.speakers_found} speakers, ${result.segments_updated} segments updated`);

    } catch (e) {
        btn.textContent = 'Error';
        showNotification('Error: ' + e.message, 'error');
        setTimeout(() => {
            btn.textContent = originalText;
            btn.disabled = false;
        }, 3000);
    }
}

async function undoDiarization() {
    const meeting = views.meeting;
    if (!meeting.id) return;

    if (!confirm('Undo all diarization? Both mic and system tracks will be reset to original.')) {
        return;
    }

    const btn = document.querySelector('.btn-undo-diarize');
    if (btn) {
        btn.textContent = 'Restoring...';
        btn.disabled = true;
    }

    try {
        const response = await fetch(`/api/meetings/${meeting.id}/undo-diarize`, {
            method: 'POST'
        });

        if (!response.ok) {
            const error = await response.json();
            throw new Error(error.detail || 'Undo failed');
        }

        // Reload meeting to show restored transcript
        await meeting.load(meeting.id);

        showNotification('Diarization undone, original transcript restored');

    } catch (e) {
        showNotification('Error: ' + e.message, 'error');
        if (btn) {
            btn.textContent = 'Undo All Diarization';
            btn.disabled = false;
        }
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
        z-index: 1000;
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
let activeAiTab = 'claude';
let aiEventSource = null;
let incrementPollTimer = null;
let _incrementState = null;  // {meetingId, incrementsByPrompt, promptNames, currentPromptId, currentVersion}

async function analyzeIncrement() {
    const meeting = views.meeting;
    if (!meeting.id) return;

    // Fetch prompts and stored increments in parallel
    try {
        const [prompts, increments] = await Promise.all([
            api.getPrompts(),
            api.getIncrements(meeting.id)
        ]);

        const promptIds = Object.keys(increments);
        if (promptIds.length > 0) {
            showStoredIncrements(meeting.id, increments, prompts);
            return;
        }
    } catch (e) {
        console.error('Failed to fetch stored increments:', e);
    }

    // Fall back to SSE generation
    generateIncrementSSE(meeting.id);
}

function generateIncrementSSE(meetingId) {
    // Close any existing connection
    if (aiEventSource) {
        aiEventSource.close();
        aiEventSource = null;
    }
    if (incrementPollTimer) {
        clearInterval(incrementPollTimer);
        incrementPollTimer = null;
    }

    // Reset state
    aiResponses = {};
    aiProviderNames = {};
    activeAiTab = 'claude';

    // Show modal with SSE UI
    const modal = document.getElementById('ai-modal');
    modal.style.display = 'flex';

    modal.querySelector('.modal-header h3').textContent = 'AI Analysis';
    modal.querySelector('.modal-tabs').innerHTML =
        '<button class="tab-btn active" data-provider="claude" onclick="switchAiTab(\'claude\')">Claude</button>' +
        '<button class="tab-btn" data-provider="codex" onclick="switchAiTab(\'codex\')">Codex</button>';
    modal.querySelector('.modal-body').innerHTML =
        '<div id="ai-response-claude" class="ai-tab-content active"><div class="loading">Connecting to Claude...</div></div>' +
        '<div id="ai-response-codex" class="ai-tab-content"><div class="loading">Connecting to Codex...</div></div>';
    modal.querySelector('.modal-footer').innerHTML =
        '<button class="btn" onclick="copyAiResponse()">Copy</button>' +
        '<button class="btn" onclick="closeAiModal()">Close</button>';

    // Use SSE for streaming
    const url = `/api/meetings/${meetingId}/analyze-stream?question=increment`;
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

function showStoredIncrements(meetingId, incrementsByPrompt, prompts) {
    if (incrementPollTimer) {
        clearInterval(incrementPollTimer);
        incrementPollTimer = null;
    }
    if (aiEventSource) {
        aiEventSource.close();
        aiEventSource = null;
    }

    // Build prompt name lookup
    const promptNames = {};
    (prompts || []).forEach(function(p) { promptNames[p.id] = p.name; });

    const promptIds = Object.keys(incrementsByPrompt);
    if (promptIds.length === 0) return;

    const modal = document.getElementById('ai-modal');
    modal.style.display = 'flex';

    modal.querySelector('.modal-header h3').textContent = 'AI Analysis';

    // Level 1: prompt tabs
    const promptTabsHtml = promptIds.map(function(pid, i) {
        var name = promptNames[pid] || pid;
        var isActive = i === 0 ? ' active' : '';
        return '<button class="tab-btn' + isActive + '" data-prompt-id="' + pid +
            '" onclick="switchPromptTab(\'' + meetingId + '\', \'' + pid + '\')">' +
            escapeHtml(name) + '</button>';
    }).join('');
    modal.querySelector('.modal-tabs').innerHTML = promptTabsHtml;

    // Body: version sub-tabs + content area
    modal.querySelector('.modal-body').innerHTML =
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
        currentPromptId: promptIds[0],
        currentVersion: null
    };

    // Load first prompt
    switchPromptTab(meetingId, promptIds[0]);
}

function switchPromptTab(meetingId, promptId) {
    // Update Level 1 prompt tab active state
    document.querySelectorAll('#ai-modal .modal-tabs .tab-btn').forEach(function(btn) {
        btn.classList.toggle('active', btn.dataset.promptId === promptId);
    });

    if (_incrementState) {
        _incrementState.currentPromptId = promptId;
    }

    var metas = (_incrementState && _incrementState.incrementsByPrompt[promptId]) || [];
    var versionTabsContainer = document.getElementById('version-tabs');

    if (metas.length === 0) {
        if (versionTabsContainer) versionTabsContainer.innerHTML = '';
        document.getElementById('increment-content').innerHTML =
            '<div class="empty">No versions generated yet</div>';
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
                '" onclick="switchIncrementVersion(\'' + meetingId + '\', \'' + promptId + '\', ' + inc.version +
                ')">v' + inc.version + ' <small style="opacity:.6">' + timeStr + '</small>' +
                statusIcon + '</button>';
        }).join('');
    }

    loadIncrementContent(meetingId, promptId, latestVersion);
}

function switchIncrementVersion(meetingId, promptId, version) {
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
    const endIdx = text.indexOf('---', 3);
    if (endIdx === -1) return text;
    return text.substring(endIdx + 3).trim();
}

async function loadIncrementContent(meetingId, promptId, version) {
    if (incrementPollTimer) {
        clearInterval(incrementPollTimer);
        incrementPollTimer = null;
    }

    const container = document.getElementById('increment-content');
    if (!container) return;

    container.innerHTML = '<div class="loading">Loading...</div>';

    try {
        const result = await api.getIncrement(meetingId, promptId, version);
        if (!result) {
            container.innerHTML = '<div class="error">Version not found</div>';
            return;
        }

        const { meta, content } = result;

        if (meta.status === 'failed') {
            const errorMsg = meta.error || 'Generation failed';
            container.innerHTML = '<div class="error">Generation failed: ' + escapeHtml(errorMsg) + '</div>';
            aiResponses['increment'] = '';
            activeAiTab = 'increment';
            return;
        }

        // status === 'completed'
        const strippedContent = stripYamlFrontmatter(content);
        const rendered = renderMarkdown(strippedContent);
        const durationStr = meta.duration_seconds != null ? meta.duration_seconds.toFixed(1) + 's' : '';
        const promptName = (_incrementState && _incrementState.promptNames[promptId]) || promptId;
        const infoStr = promptName + ' · v' + meta.version + ' · ' + meta.segment_count + ' segments' +
            (durationStr ? ' · ' + durationStr : '');

        container.innerHTML =
            '<div class="ai-answer">' + rendered + '</div>' +
            '<div class="ai-provider">' + escapeHtml(infoStr) + '</div>';

        aiResponses['increment'] = strippedContent;
        activeAiTab = 'increment';
    } catch (e) {
        container.innerHTML = '<div class="error">Failed to load: ' + escapeHtml(e.message) + '</div>';
    }
}

function regeneratePrompt() {
    if (incrementPollTimer) {
        clearInterval(incrementPollTimer);
        incrementPollTimer = null;
    }
    var meetingId = _incrementState ? _incrementState.meetingId : (views.meeting && views.meeting.id);
    if (meetingId) {
        generateIncrementSSE(meetingId);
    }
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
    // Clear increment polling if active
    if (incrementPollTimer) {
        clearInterval(incrementPollTimer);
        incrementPollTimer = null;
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


// --- Anchor / WDIM ---

let anchorRequestInFlight = false;
let anchorLastRequestAt = 0;
let lastAnchorPayload = null;
const ANCHOR_MIN_INTERVAL_MS = 8000;

function isTextInputActive() {
    const el = document.activeElement;
    if (!el) return false;
    const tag = el.tagName;
    return tag === 'INPUT' || tag === 'TEXTAREA' || el.isContentEditable;
}

function isLiveViewActive() {
    const view = document.getElementById('view-live');
    return view && view.style.display !== 'none';
}

function openAnchor() {
    if (!isLiveViewActive()) return;
    const overlay = document.getElementById('anchor-overlay');
    if (!overlay) return;

    overlay.style.display = 'flex';
    if (lastAnchorPayload && Date.now() - anchorLastRequestAt < ANCHOR_MIN_INTERVAL_MS) {
        renderAnchorData(lastAnchorPayload);
        return;
    }
    renderAnchorLoading();
    requestAnchor();
}

function closeAnchor() {
    const overlay = document.getElementById('anchor-overlay');
    if (overlay) overlay.style.display = 'none';
}

async function requestAnchor() {
    if (anchorRequestInFlight) return;

    const now = Date.now();
    if (now - anchorLastRequestAt < ANCHOR_MIN_INTERVAL_MS) {
        const noteEl = document.getElementById('anchor-note');
        if (noteEl) {
            noteEl.textContent = 'Please wait a few seconds before requesting again.';
        }
        return;
    }

    anchorRequestInFlight = true;
    anchorLastRequestAt = now;

    const btn = document.getElementById('anchor-btn');
    if (btn) btn.disabled = true;

    try {
        const payload = {
            window_seconds: 240,
            max_segments: 35
        };
        if (views.live.currentMeeting?.id) {
            payload.meeting_id = views.live.currentMeeting.id;
        }

        const response = await api.getLiveAnchor(payload);
        renderAnchorData(response);
    } catch (e) {
        renderAnchorError(e.message);
    } finally {
        anchorRequestInFlight = false;
        if (btn) btn.disabled = false;
    }
}

function renderAnchorLoading() {
    const toneEl = document.getElementById('anchor-tone');
    const topicEl = document.getElementById('anchor-topic-text');
    const bulletsEl = document.getElementById('anchor-bullets');
    const mentionEl = document.getElementById('anchor-mention');
    const noteEl = document.getElementById('anchor-note');

    if (toneEl) {
        toneEl.className = 'anchor-tone tone-neutral';
        toneEl.textContent = 'neutral';
    }
    if (topicEl) topicEl.textContent = 'Loading...';
    if (bulletsEl) {
        bulletsEl.innerHTML = '';
        const li = document.createElement('li');
        li.className = 'anchor-loading';
        li.textContent = 'Loading context...';
        bulletsEl.appendChild(li);
    }
    if (mentionEl) mentionEl.textContent = '';
    if (noteEl) noteEl.textContent = '';
}

function renderAnchorError(message) {
    const topicEl = document.getElementById('anchor-topic-text');
    const bulletsEl = document.getElementById('anchor-bullets');
    const mentionEl = document.getElementById('anchor-mention');
    const noteEl = document.getElementById('anchor-note');

    if (topicEl) topicEl.textContent = 'Context unavailable';
    if (bulletsEl) {
        bulletsEl.innerHTML = '';
        const li = document.createElement('li');
        li.className = 'anchor-loading';
        li.textContent = message || 'Failed to load context';
        bulletsEl.appendChild(li);
    }
    if (mentionEl) mentionEl.textContent = '';
    if (noteEl) noteEl.textContent = '';
}

function renderAnchorData(payload) {
    const data = payload?.data || payload || {};
    lastAnchorPayload = payload;
    const rawTone = (data.topic_tone || 'neutral').toLowerCase();
    const tone = ['neutral', 'calm', 'heated'].includes(rawTone) ? rawTone : 'neutral';
    const toneEl = document.getElementById('anchor-tone');
    const topicEl = document.getElementById('anchor-topic-text');
    const bulletsEl = document.getElementById('anchor-bullets');
    const mentionEl = document.getElementById('anchor-mention');
    const noteEl = document.getElementById('anchor-note');

    if (toneEl) {
        toneEl.className = `anchor-tone tone-${tone}`;
        toneEl.textContent = tone;
    }
    if (topicEl) {
        topicEl.textContent = data.current_topic || 'No recent context.';
    }

    if (bulletsEl) {
        bulletsEl.innerHTML = '';
        const bullets = Array.isArray(data.summary_bullets) ? data.summary_bullets : [];
        if (bullets.length === 0) {
            const li = document.createElement('li');
            li.className = 'anchor-loading';
            li.textContent = 'No recent points.';
            bulletsEl.appendChild(li);
        } else {
            for (const bullet of bullets) {
                const li = document.createElement('li');
                li.className = 'anchor-bullet';

                const tsDisplay = bullet.ts_display || '';
                if (tsDisplay) {
                    const tsBtn = document.createElement('button');
                    tsBtn.type = 'button';
                    tsBtn.className = 'anchor-ts';
                    tsBtn.textContent = tsDisplay;
                    tsBtn.dataset.tsDisplay = tsDisplay;
                    tsBtn.addEventListener('click', () => {
                        scrollToLiveTimestamp(tsDisplay);
                    });
                    li.appendChild(tsBtn);
                } else {
                    const tsSpan = document.createElement('span');
                    tsSpan.className = 'anchor-ts anchor-ts-empty';
                    tsSpan.textContent = '--:--:--';
                    li.appendChild(tsSpan);
                }

                const textSpan = document.createElement('span');
                textSpan.className = 'anchor-bullet-text';
                textSpan.textContent = bullet.text || '';
                li.appendChild(textSpan);

                bulletsEl.appendChild(li);
            }
        }
    }

    if (mentionEl) {
        mentionEl.innerHTML = '';
        const userMentioned = Boolean(data.user_mentioned);
        mentionEl.className = `anchor-mention ${userMentioned ? 'mention-alert' : 'mention-ok'}`;

        if (userMentioned) {
            const label = document.createElement('span');
            label.textContent = 'Mentioned';
            mentionEl.appendChild(label);

            if (data.mention_ts_display) {
                const tsBtn = document.createElement('button');
                tsBtn.type = 'button';
                tsBtn.className = 'anchor-ts';
                tsBtn.textContent = data.mention_ts_display;
                tsBtn.addEventListener('click', () => {
                    scrollToLiveTimestamp(data.mention_ts_display);
                });
                mentionEl.appendChild(tsBtn);
            }

            if (data.mention_context) {
                const ctx = document.createElement('span');
                ctx.className = 'anchor-mention-context';
                ctx.textContent = data.mention_context;
                mentionEl.appendChild(ctx);
            }
        } else {
            mentionEl.textContent = 'No mentions for you in the last minutes.';
        }
    }

    if (noteEl) {
        noteEl.textContent = payload?.approximate ? 'Summary may be approximate.' : '';
    }
}

function escapeCssSelector(value) {
    if (window.CSS && CSS.escape) return CSS.escape(value);
    return value.replace(/["\\]/g, '\\$&');
}

function scrollToLiveTimestamp(tsDisplay) {
    if (!tsDisplay) return;
    const container = document.getElementById('live-transcript');
    if (!container) return;

    const selector = `[data-ts-display="${escapeCssSelector(tsDisplay)}"]`;
    const target = container.querySelector(selector);
    if (!target) return;

    target.scrollIntoView({ behavior: 'smooth', block: 'center' });
    target.classList.add('segment-highlight');
    setTimeout(() => target.classList.remove('segment-highlight'), 1600);
}

function initAnchorOverlay() {
    const overlay = document.getElementById('anchor-overlay');
    if (overlay) {
        overlay.addEventListener('click', (e) => {
            if (e.target === overlay) closeAnchor();
        });
    }

    document.addEventListener('keydown', (e) => {
        if (e.key === 'Escape') {
            closeAnchor();
            return;
        }

        if (isTextInputActive()) return;

        if (e.altKey && e.key.toLowerCase() === 'j') {
            e.preventDefault();
            openAnchor();
        }
    });
}


// --- Speaker Management ---

function formatTime(seconds) {
    const m = Math.floor(seconds / 60);
    const s = Math.floor(seconds % 60);
    return `${m}:${s.toString().padStart(2, '0')}`;
}

function toggleEmbeddings(speakerId) {
    const list = document.getElementById(`embeddings-${speakerId}`);
    if (list) {
        const isHidden = list.style.display === 'none';
        list.style.display = isHidden ? 'block' : 'none';

        // Update button text
        const card = list.closest('.speaker-card');
        const btn = card?.querySelector('.btn-toggle');
        if (btn) {
            btn.textContent = isHidden ? 'Hide Details' : 'Show Details';
        }
    }
}

async function renameSpeaker(speakerId, currentName) {
    const newName = prompt('Enter new name for speaker:', currentName);
    if (!newName || newName === currentName) return;

    try {
        const res = await fetch(`/api/speakers/${encodeURIComponent(speakerId)}`, {
            method: 'PUT',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ display_name: newName })
        });
        if (!res.ok) {
            const error = await res.json().catch(() => ({}));
            throw new Error(error.detail || 'Failed to rename speaker');
        }
        showNotification(`Speaker renamed to "${newName}"`, 'success');
        views.speakers.load();
    } catch (e) {
        showNotification(`Error: ${e.message}`, 'error');
    }
}

async function deleteSpeaker(speakerId, displayName) {
    if (!confirm(`Delete speaker "${displayName}" and all their embeddings?`)) return;

    try {
        const res = await fetch(`/api/speakers/${encodeURIComponent(speakerId)}`, {
            method: 'DELETE'
        });
        if (!res.ok) {
            const error = await res.json().catch(() => ({}));
            throw new Error(error.detail || 'Failed to delete speaker');
        }
        showNotification(`Speaker "${displayName}" deleted`, 'success');
        views.speakers.load();
    } catch (e) {
        showNotification(`Error: ${e.message}`, 'error');
    }
}

async function deleteEmbedding(speakerId, index) {
    if (!confirm('Delete this embedding?')) return;

    try {
        const res = await fetch(`/api/speakers/${encodeURIComponent(speakerId)}/embeddings/${index}`, {
            method: 'DELETE'
        });
        if (!res.ok) {
            const error = await res.json().catch(() => ({}));
            throw new Error(error.detail || 'Failed to delete embedding');
        }
        showNotification('Embedding deleted', 'success');
        views.speakers.load();
    } catch (e) {
        showNotification(`Error: ${e.message}`, 'error');
    }
}


// --- Enrollment Functions ---

function toggleEnrollMode() {
    views.meeting.toggleEnrollMode();
}

function toggleSegmentSelection(...indices) {
    const meeting = views.meeting;
    for (const idx of indices) {
        const i = meeting.selectedSegments.indexOf(idx);
        if (i === -1) {
            meeting.selectedSegments.push(idx);
        } else {
            meeting.selectedSegments.splice(i, 1);
        }
    }

    // Update segment visual state
    const transcriptEl = document.getElementById('transcript');
    transcriptEl.querySelectorAll('.transcript-segment').forEach(el => {
        const segIndices = (el.dataset.segmentIndices || '').split(',').map(Number);
        const isSelected = segIndices.some(idx => meeting.selectedSegments.includes(idx));
        el.classList.toggle('segment-selected', isSelected);
    });

    // Update enrollment bar
    meeting.renderEnrollmentBar();
}

function clearEnrollSelection() {
    views.meeting.selectedSegments = [];
    const transcriptEl = document.getElementById('transcript');
    transcriptEl.querySelectorAll('.transcript-segment').forEach(el => {
        el.classList.remove('segment-selected');
        const checkbox = el.querySelector('input[type="checkbox"]');
        if (checkbox) checkbox.checked = false;
    });
    views.meeting.renderEnrollmentBar();
}

async function openEnrollModal() {
    const meeting = views.meeting;
    if (meeting.selectedSegments.length === 0) return;

    // Update segment count
    document.getElementById('enroll-segment-count').textContent = meeting.selectedSegments.length;

    // Load existing speakers for dropdown
    try {
        const res = await fetch('/api/speakers');
        if (res.ok) {
            const speakers = await res.json();
            const select = document.getElementById('existing-speaker-select');
            select.innerHTML = speakers.map(s =>
                `<option value="${escapeHtml(s.id)}">${escapeHtml(s.display_name)}</option>`
            ).join('');

            if (speakers.length === 0) {
                select.innerHTML = '<option value="">No speakers available</option>';
            }
        }
    } catch (e) {
        console.error('Failed to load speakers:', e);
    }

    // Reset form
    document.getElementById('enroll-speaker-name').value = '';
    document.querySelector('input[name="speaker-type"][value="new"]').checked = true;
    toggleSpeakerType();

    // Show modal
    document.getElementById('enroll-modal').style.display = 'flex';
}

function closeEnrollModal() {
    document.getElementById('enroll-modal').style.display = 'none';
}

function toggleSpeakerType() {
    const isNew = document.querySelector('input[name="speaker-type"]:checked').value === 'new';
    document.getElementById('new-speaker-form').style.display = isNew ? 'block' : 'none';
    document.getElementById('existing-speaker-form').style.display = isNew ? 'none' : 'block';
}

async function submitEnrollment() {
    const meeting = views.meeting;
    const isNew = document.querySelector('input[name="speaker-type"]:checked').value === 'new';
    const track = document.getElementById('enroll-track-select').value;

    let speakerId = null;
    let speakerName = null;

    if (isNew) {
        speakerName = document.getElementById('enroll-speaker-name').value.trim();
        if (!speakerName) {
            showNotification('Please enter a speaker name', 'error');
            return;
        }
    } else {
        speakerId = document.getElementById('existing-speaker-select').value;
        if (!speakerId) {
            showNotification('Please select a speaker', 'error');
            return;
        }
    }

    // Collect segment IDs (we need the actual segment indices from the original array)
    const segmentIndices = meeting.selectedSegments;

    // Build request payload
    const payload = {
        meeting_id: meeting.id,
        segment_indices: segmentIndices,
        track: track
    };

    if (isNew) {
        payload.display_name = speakerName;
    } else {
        payload.speaker_id = speakerId;
    }

    // Disable submit button
    const submitBtn = document.getElementById('enroll-submit-btn');
    submitBtn.disabled = true;
    submitBtn.textContent = 'Enrolling...';

    try {
        const res = await fetch('/api/speakers/enroll-from-segments', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(payload)
        });

        if (!res.ok) {
            const error = await res.json().catch(() => ({}));
            throw new Error(error.detail || 'Enrollment failed');
        }

        const result = await res.json();
        showNotification(`Speaker enrolled: ${result.display_name}`, 'success');

        // Close modal and clear selection
        closeEnrollModal();
        clearEnrollSelection();
        views.meeting.toggleEnrollMode();

    } catch (e) {
        showNotification(`Error: ${e.message}`, 'error');
    } finally {
        submitBtn.disabled = false;
        submitBtn.textContent = 'Add Speaker';
    }
}


// --- Init ---

document.addEventListener('DOMContentLoaded', () => {
    router.init();
    initAnchorOverlay();
});
