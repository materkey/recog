/* global renderMarkdown, escapeHtml, views */
// Questions about the open meeting: a panel beside its transcript. The server keeps
// the dialog; only questions on their way live here, per meeting, so that moving to
// another meeting neither loses nor mixes them.
(() => {
    const requests = new Map(); // meeting id -> { question, error }
    let meetingId = null;
    let turns = [];
    let isOpen = false;
    let loadGeneration = 0;
    let citedElement = null;

    const byId = id => document.getElementById(id);

    function routeMeetingId() {
        const match = window.location.hash.match(/^#\/meeting\/(.+)$/);
        return match ? match[1] : null;
    }

    function chatUrl(id) {
        return `/api/meetings/${encodeURIComponent(id)}/chat`;
    }

    async function requestJson(url, options) {
        const response = await fetch(url, options);
        if (response.ok) return response.json();
        const body = await response.json().catch(() => ({}));
        throw new Error(body.detail || `HTTP ${response.status}`);
    }

    function renderAnswer(text) {
        return renderMarkdown(text).replace(
            /\[?\b(\d{1,2}:\d{2}:\d{2})\b\]?/g,
            '<button type="button" class="chat-cite" data-ts="$1" title="Показать в транскрипте">$1</button>',
        );
    }

    function renderTurn(turn) {
        const cut = turn.transcript_truncated
            ? '<div class="chat-note">Встреча очень длинная: модель видела только её конец.</div>'
            : '';
        return `<div class="chat-turn">
            <div class="chat-question">${escapeHtml(turn.question)}</div>
            <div class="chat-answer ai-answer">${renderAnswer(turn.answer)}</div>
            ${cut}
            <div class="chat-provider">${escapeHtml(turn.provider)}</div>
        </div>`;
    }

    function renderWaiting(question) {
        return `<div class="chat-turn">
            <div class="chat-question">${escapeHtml(question)}</div>
            <div class="meeting-chat-waiting"><span class="streaming-indicator">●</span> Ищу ответ в транскрипте…</div>
        </div>`;
    }

    function render() {
        const request = requests.get(meetingId);
        const running = Boolean(request) && !request.error;
        const log = byId('meeting-chat-log');
        const parts = turns.map(renderTurn);
        if (running) parts.push(renderWaiting(request.question));
        log.innerHTML = parts.join('') || '<p class="chat-empty">Спросите что угодно об этой встрече: кто что говорил, что решили, какие остались вопросы. В ответе будут ссылки на реплики транскрипта.</p>';
        byId('meeting-chat-send').disabled = running;
        byId('meeting-chat-clear').hidden = running || !turns.length;
        showError(request?.error || '');
        const last = log.querySelectorAll('.chat-turn');
        if (last.length) last[last.length - 1].scrollIntoView({ block: 'start' });
    }

    function showError(message) {
        const error = byId('meeting-chat-error');
        error.textContent = message;
        error.hidden = !message;
    }

    async function loadDialog() {
        const generation = ++loadGeneration;
        const id = meetingId;
        turns = [];
        render();
        try {
            const loaded = await requestJson(chatUrl(id));
            if (generation !== loadGeneration) return;
            turns = loaded;
            render();
        } catch (error) {
            if (generation === loadGeneration) showError(error.message);
        }
    }

    function restoreFailedQuestion() {
        const request = requests.get(meetingId);
        if (request?.error) byId('meeting-chat-input').value = request.question;
    }

    function followRoute() {
        const id = routeMeetingId();
        if (!id || id === meetingId) return;
        meetingId = id;
        turns = [];
        byId('meeting-chat-input').value = '';
        if (!isOpen) return;
        restoreFailedQuestion();
        loadDialog();
    }

    async function ask(question) {
        const id = meetingId;
        requests.set(id, { question });
        byId('meeting-chat-input').value = '';
        render();
        try {
            const turn = await requestJson(chatUrl(id), {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ question }),
            });
            requests.delete(id);
            if (id !== meetingId) return;
            turns.push(turn);
            render();
        } catch (error) {
            requests.set(id, { question, error: error.message });
            if (id !== meetingId) return;
            restoreFailedQuestion();
            render();
        }
    }

    function submit() {
        const input = byId('meeting-chat-input');
        const question = input.value.trim();
        const request = requests.get(meetingId);
        if (!question || (request && !request.error)) return;
        ask(question);
    }

    async function clearDialog() {
        try {
            await requestJson(chatUrl(meetingId), { method: 'DELETE' });
            requests.delete(meetingId);
            turns = [];
            render();
        } catch (error) {
            showError(error.message);
        }
    }

    function setOpen(open) {
        isOpen = open;
        byId('meeting-chat').hidden = !open;
        byId('view-meeting').classList.toggle('chat-open', open);
        byId('meeting-chat-btn').setAttribute('aria-expanded', String(open));
        if (open) {
            restoreFailedQuestion();
            loadDialog();
            byId('meeting-chat-input').focus();
        } else {
            byId('meeting-chat-btn').focus();
        }
    }

    function secondsOf(display) {
        const [hours, minutes, seconds] = display.split(':').map(Number);
        return hours * 3600 + minutes * 60 + seconds;
    }

    // The model may cite a time slightly off, and a replica is a group of segments.
    function nearestSegmentIndex(segments, ts) {
        const target = secondsOf(ts);
        let best = -1;
        let bestDistance = Infinity;
        segments.forEach((segment, index) => {
            if (segment.event_type || !segment.ts_display) return;
            const distance = Math.abs(secondsOf(segment.ts_display) - target);
            if (distance < bestDistance) {
                best = index;
                bestDistance = distance;
            }
        });
        return best;
    }

    function showInTranscript(ts) {
        const search = byId('transcript-search');
        // A filtered transcript numbers its groups by the filtered list, and may lack the replica.
        if (search.value) {
            search.value = '';
            views.meeting.filterTranscript('');
        }
        const index = String(nearestSegmentIndex(views.meeting.segments, ts));
        const groups = [...document.querySelectorAll('#transcript .transcript-segment')];
        const target = groups.find(group => group.dataset.segmentIndices.split(',').includes(index));
        if (!target) return;
        citedElement?.classList.remove('segment-cited');
        citedElement = target;
        target.classList.add('segment-cited');
        target.scrollIntoView({ block: 'center' });
    }

    function onInputKeydown(event) {
        if (event.key !== 'Enter' || event.shiftKey) return;
        event.preventDefault();
        // Enter confirming an IME conversion; keyCode 229 covers Safari's event order.
        if (event.isComposing || event.keyCode === 229) return;
        byId('meeting-chat-form').requestSubmit();
    }

    function init() {
        meetingId = routeMeetingId();
        window.addEventListener('hashchange', followRoute);
        byId('meeting-chat-btn').addEventListener('click', () => setOpen(!isOpen));
        byId('meeting-chat-close').addEventListener('click', () => setOpen(false));
        byId('meeting-chat-clear').addEventListener('click', clearDialog);
        byId('meeting-chat-input').addEventListener('keydown', onInputKeydown);
        byId('meeting-chat-form').addEventListener('submit', event => {
            event.preventDefault();
            submit();
        });
        byId('meeting-chat').addEventListener('keydown', event => {
            if (event.key === 'Escape') setOpen(false);
        });
        byId('meeting-chat-log').addEventListener('click', event => {
            const citation = event.target.closest('.chat-cite');
            if (citation) showInTranscript(citation.dataset.ts);
        });
    }

    document.addEventListener('DOMContentLoaded', init);
})();
