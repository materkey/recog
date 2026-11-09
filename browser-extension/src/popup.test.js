/**
 * The extension popup: what it shows for the state content.js keeps in
 * chrome.storage.local.
 *
 * @jest-environment jsdom
 */

import { jest } from '@jest/globals';
import { readFileSync } from 'node:fs';

const popupHtml = readFileSync(new URL('../popup.html', import.meta.url), 'utf8');
const body = popupHtml.slice(popupHtml.indexOf('<body>') + 6, popupHtml.indexOf('<script'));

let stored = {};
let storageListener = null;

globalThis.chrome = {
  storage: {
    local: {
      get: jest.fn((keys, callback) => callback(stored)),
    },
    onChanged: {
      addListener: jest.fn((listener) => { storageListener = listener; }),
    },
  },
};

const $ = (id) => document.getElementById(id);

beforeAll(async () => {
  jest.useFakeTimers();
  document.body.innerHTML = body;
  await import('../popup.js');
});

afterAll(() => {
  jest.useRealTimers();
});

function show(data) {
  stored = data;
  storageListener({}, 'local');
}

describe('popup', () => {
  test('opened with no conference - DOMContentLoaded - shows the empty states', () => {
    stored = {};
    document.dispatchEvent(new Event('DOMContentLoaded'));

    expect($('statusDot').classList.contains('connected')).toBe(false);
    expect($('statusText').textContent).toBe('Not in a conference');
    expect($('currentSpeakerName').textContent).toBe('-');
    expect($('participantCount').textContent).toBe('0');
    expect($('participantsList').textContent).toContain('No participants');
    expect($('historyList').textContent).toContain('No history yet');
    expect($('transcriptCount').textContent).toBe('0');
    expect($('transcriptList').textContent).toContain('No transcript yet');
  });

  test('a current speaker - storage changes - shows the speaker and the connected dot', () => {
    show({ currentSpeaker: { name: 'Alice Smith', id: 'p1' } });

    expect($('statusDot').classList.contains('connected')).toBe(true);
    expect($('statusText').textContent).toBe('Connected to conference');
    expect($('currentSpeakerName').textContent).toBe('Alice Smith');
    expect($('currentSpeakerId').textContent).toBe('p1');
  });

  test('a current speaker without name or id - storage changes - shows No one', () => {
    show({ currentSpeaker: {} });

    expect($('currentSpeakerName').textContent).toBe('No one');
    expect($('currentSpeakerId').textContent).toBe('');
  });

  test('connected with nobody speaking - storage changes - says so', () => {
    show({ connected: true });

    expect($('statusDot').classList.contains('connected')).toBe(true);
    expect($('statusText').textContent).toBe('Connected (no one speaking)');
    expect($('currentSpeakerName').textContent).toBe('-');
  });

  test('a storage change in another namespace - fired - does not reload', () => {
    show({ connected: true });
    stored = {};
    storageListener({}, 'sync');

    expect($('statusText').textContent).toBe('Connected (no one speaking)');
  });

  test('participants - storage changes - lists them with initials and marks the local one', () => {
    show({
      participants: [
        { name: 'alice smith', isLocal: true },
        { name: '' },
      ],
    });

    expect($('participantCount').textContent).toBe('2');
    const items = $('participantsList').querySelectorAll('li');
    expect(items).toHaveLength(2);
    expect(items[0].querySelector('.participant-avatar').textContent).toBe('AS');
    expect(items[0].querySelector('.participant-local').textContent).toBe('You');
    expect(items[1].querySelector('.participant-avatar').textContent).toBe('?');
    expect(items[1].querySelector('.participant-local')).toBeNull();
  });

  test('twelve history entries - storage changes - shows the ten most recent', () => {
    const speakerHistory = Array.from({ length: 12 }, (_, i) => ({ name: `S${i}`, time: `10:00:${i}` }));
    show({ speakerHistory });

    const items = $('historyList').querySelectorAll('li');
    expect(items).toHaveLength(10);
    expect(items[0].textContent).toContain('S0');
    expect(items[0].querySelector('.history-time').textContent).toBe('10:00:0');
  });

  test('transcript lines - storage changes - lists them with classes and uncertainty marks', () => {
    show({
      transcriptLines: [
        { speaker: 'Me', text: 'one', time: 't1', is_local: true },
        { speaker: 'Bob', text: 'two', time: 't2', uncertain: true, uncertain_reason: 'mics_off' },
        { speaker: 'Eve', text: 'three', time: 't3', uncertain: true },
      ],
    });

    expect($('transcriptCount').textContent).toBe('3');
    const items = $('transcriptList').querySelectorAll('li');
    expect(items[0].className).toBe('local');
    expect(items[0].querySelector('.transcript-speaker').textContent).toBe('Me');
    expect(items[1].className).toBe('uncertain mics-off');
    expect(items[1].querySelector('.transcript-speaker').textContent).toBe('Bob (🔇?)');
    expect(items[2].className).toBe('uncertain');
    expect(items[2].querySelector('.transcript-speaker').textContent).toBe('Eve (?)');
    expect(items[2].querySelector('.transcript-text').textContent).toBe('three');
  });

  test('a later change without transcript lines - storage changes - keeps the last transcript', () => {
    show({ transcriptLines: [{ speaker: 'Me', text: 'kept', time: 't1' }] });
    show({ connected: true });

    expect($('transcriptCount').textContent).toBe('1');
    expect($('transcriptList').textContent).toContain('kept');
  });

  test('transcript lines - copy button clicked - copies markdown and flashes Copied', async () => {
    show({
      transcriptLines: [
        { speaker: 'Me', text: 'one', time: 't1' },
        { speaker: 'Bob', text: 'two', time: 't2', uncertain: true },
      ],
    });
    navigator.clipboard.writeText.mockClear();

    $('copyTranscriptBtn').click();
    await Promise.resolve();

    expect(navigator.clipboard.writeText).toHaveBeenCalledWith('**Me** *[t1]*: one\n\n**Bob (?)** *[t2]*: two');
    const label = $('copyTranscriptBtn').querySelector('.copy-text');
    expect(label.textContent).toBe('Copied!');
    expect($('copyTranscriptBtn').classList.contains('copied')).toBe(true);
    jest.advanceTimersByTime(2000);
    expect(label.textContent).toBe('Copy all');
    expect($('copyTranscriptBtn').classList.contains('copied')).toBe(false);
  });

  test('the transcript tab button - clicked - activates its tab and content', () => {
    const transcriptBtn = document.querySelector('.tab-btn[data-tab="transcript"]');
    transcriptBtn.click();

    expect(transcriptBtn.classList.contains('active')).toBe(true);
    expect(document.querySelector('.tab-btn[data-tab="info"]').classList.contains('active')).toBe(false);
    expect($('tab-transcript').classList.contains('active')).toBe(true);
    expect($('tab-info').classList.contains('active')).toBe(false);
  });

  test('the one-second refresh - elapses - reloads from storage', () => {
    stored = { connected: true };
    storageListener({}, 'local');
    stored = {};
    jest.advanceTimersByTime(1000);

    expect($('statusText').textContent).toBe('Not in a conference');
  });

  test('an empty transcript - copy button clicked - copies nothing', () => {
    show({ transcriptLines: [] });
    navigator.clipboard.writeText.mockClear();

    $('copyTranscriptBtn').click();

    expect(navigator.clipboard.writeText).not.toHaveBeenCalled();
  });

  test('names, times and text holding markup - storage changes - shown as text, no element created', () => {
    show({
      connected: true,
      participants: [{ name: '<img src=x onerror="window.pwned=1">', isLocal: false }],
      speakerHistory: [{ name: '<b>Bob</b>', time: '<i>12:00</i>' }],
      transcriptLines: [{ speaker: '<u>Carol</u>', time: '<s>12:01</s>', text: '<em>hello</em> & bye' }],
    });

    for (const id of ['participantsList', 'historyList', 'transcriptList']) {
      expect($(id).querySelector('img, b, i, u, s, em')).toBeNull();
    }
    expect($('participantsList').textContent).toContain('<img src=x onerror="window.pwned=1">');
    expect($('historyList').textContent).toContain('<b>Bob</b>');
    expect($('historyList').textContent).toContain('<i>12:00</i>');
    expect($('transcriptList').textContent).toContain('<u>Carol</u>');
    expect($('transcriptList').textContent).toContain('<s>12:01</s>');
    expect($('transcriptList').textContent).toContain('<em>hello</em> & bye');
  });
});
