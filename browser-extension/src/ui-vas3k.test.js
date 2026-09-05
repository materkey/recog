/**
 * TDD Tests for vas3k.cloud UI features
 */

import { state } from './state.js';

// We'll import functions as we implement them
// For now, these imports will fail (RED phase)

describe('Copy Transcript Feature', () => {
  beforeEach(() => {
    // Reset DOM
    document.body.innerHTML = '';
    // Reset clipboard mock
    navigator.clipboard.writeText.mockClear();
    // Reset state
    state.transcriptLines = [];
  });

  describe('copyVas3kTranscript', () => {
    let copyVas3kTranscript;

    beforeAll(async () => {
      // Import will fail until function exists
      const module = await import('./ui-vas3k.js');
      copyVas3kTranscript = module.copyVas3kTranscript;
    });

    test('copies formatted transcript to clipboard', async () => {
      // Setup: transcript lines exist
      state.transcriptLines = [
        { speaker: 'Alice', time: '10:00:01', text: 'Hello everyone' },
        { speaker: 'Bob', time: '10:00:05', text: 'Hi Alice' },
      ];

      await copyVas3kTranscript();

      expect(navigator.clipboard.writeText).toHaveBeenCalledTimes(1);
      const copiedText = navigator.clipboard.writeText.mock.calls[0][0];

      // Should include speaker names and text
      expect(copiedText).toContain('Alice');
      expect(copiedText).toContain('Hello everyone');
      expect(copiedText).toContain('Bob');
      expect(copiedText).toContain('Hi Alice');
    });

    test('does nothing when transcript is empty', async () => {
      state.transcriptLines = [];

      await copyVas3kTranscript();

      expect(navigator.clipboard.writeText).not.toHaveBeenCalled();
    });

    test('formats transcript with timestamps', async () => {
      state.transcriptLines = [
        { speaker: 'Alice', time: '10:00:01', text: 'Test message' },
      ];

      await copyVas3kTranscript();

      const copiedText = navigator.clipboard.writeText.mock.calls[0][0];
      expect(copiedText).toContain('10:00:01');
    });
  });

  describe('Copy button in UI', () => {
    let createFloatingPanel;

    beforeAll(async () => {
      const module = await import('./ui-vas3k.js');
      createFloatingPanel = module.createFloatingPanel;
    });

    test('panel has copy button', () => {
      createFloatingPanel();

      const copyBtn = document.querySelector('.jitsi-copy-btn');
      expect(copyBtn).not.toBeNull();
      expect(copyBtn.textContent).toContain('Copy');
    });

    test('copy button shows success state after click', async () => {
      state.transcriptLines = [
        { speaker: 'Alice', time: '10:00:01', text: 'Test' },
      ];
      createFloatingPanel();

      const copyBtn = document.querySelector('.jitsi-copy-btn');
      copyBtn.click();

      // Wait for async clipboard operation
      await new Promise(resolve => setTimeout(resolve, 10));

      // Button should show success state
      expect(copyBtn.classList.contains('copied')).toBe(true);
    });
  });
});

describe('Follow/Auto-scroll Feature', () => {
  beforeEach(() => {
    document.body.innerHTML = '';
    state.vas3kFollowEnabled = undefined;
  });

  describe('toggleVas3kFollow', () => {
    let toggleVas3kFollow, createFloatingPanel;

    beforeAll(async () => {
      const module = await import('./ui-vas3k.js');
      toggleVas3kFollow = module.toggleVas3kFollow;
      createFloatingPanel = module.createFloatingPanel;
    });

    test('toggles follow state', () => {
      state.vas3kFollowEnabled = false;

      toggleVas3kFollow();

      expect(state.vas3kFollowEnabled).toBe(true);
    });

    test('updates button visual state', () => {
      createFloatingPanel();
      state.vas3kFollowEnabled = false;

      toggleVas3kFollow();

      const followBtn = document.querySelector('.jitsi-follow-btn');
      expect(followBtn.classList.contains('active')).toBe(true);
    });
  });

  describe('auto-scroll behavior', () => {
    let addVas3kTranscriptLine, createFloatingPanel;

    beforeAll(async () => {
      const module = await import('./ui-vas3k.js');
      addVas3kTranscriptLine = module.addVas3kTranscriptLine;
      createFloatingPanel = module.createFloatingPanel;
    });

    test('scrolls to bottom when follow is enabled', () => {
      createFloatingPanel();
      state.vas3kFollowEnabled = true;

      const content = document.querySelector('.jitsi-transcript-content');
      const scrollSpy = jest.spyOn(content, 'scrollTop', 'set');

      addVas3kTranscriptLine({ speaker: 'Alice', time: '10:00', text: 'Test' });

      expect(scrollSpy).toHaveBeenCalled();
    });

    test('does not scroll when follow is disabled', () => {
      createFloatingPanel();
      state.vas3kFollowEnabled = false;

      const content = document.querySelector('.jitsi-transcript-content');
      // Add some content first
      content.innerHTML = '<div style="height: 1000px"></div>';
      content.scrollTop = 0;

      addVas3kTranscriptLine({ speaker: 'Alice', time: '10:00', text: 'Test' });

      // Scroll should not change (or at least not go to bottom)
      expect(content.scrollTop).toBe(0);
    });
  });
});

describe('Scroll Buttons Feature', () => {
  beforeEach(() => {
    document.body.innerHTML = '';
  });

  describe('scroll buttons in UI', () => {
    let createFloatingPanel;

    beforeAll(async () => {
      const module = await import('./ui-vas3k.js');
      createFloatingPanel = module.createFloatingPanel;
    });

    test('panel has scroll up button', () => {
      createFloatingPanel();

      const scrollUpBtn = document.querySelector('.jitsi-scroll-up-btn');
      expect(scrollUpBtn).not.toBeNull();
    });

    test('panel has scroll down button', () => {
      createFloatingPanel();

      const scrollDownBtn = document.querySelector('.jitsi-scroll-down-btn');
      expect(scrollDownBtn).not.toBeNull();
    });

    test('scroll up button scrolls to top', () => {
      createFloatingPanel();
      const content = document.querySelector('.jitsi-transcript-content');
      content.innerHTML = '<div style="height: 1000px"></div>';
      content.scrollTop = 500;

      const scrollUpBtn = document.querySelector('.jitsi-scroll-up-btn');
      scrollUpBtn.click();

      expect(content.scrollTop).toBe(0);
    });

    test('scroll down button scrolls to bottom', () => {
      createFloatingPanel();
      const content = document.querySelector('.jitsi-transcript-content');
      content.innerHTML = '<div style="height: 1000px"></div>';
      content.scrollTop = 0;

      const scrollDownBtn = document.querySelector('.jitsi-scroll-down-btn');
      scrollDownBtn.click();

      expect(content.scrollTop).toBe(content.scrollHeight - content.clientHeight);
    });
  });
});

describe('Highlight Feature', () => {
  beforeEach(() => {
    document.body.innerHTML = '';
  });

  describe('highlightVas3kLine', () => {
    let highlightVas3kLine, addVas3kTranscriptLine, createFloatingPanel;

    beforeAll(async () => {
      const module = await import('./ui-vas3k.js');
      highlightVas3kLine = module.highlightVas3kLine;
      addVas3kTranscriptLine = module.addVas3kTranscriptLine;
      createFloatingPanel = module.createFloatingPanel;
    });

    test('adds highlight class to specified line', () => {
      createFloatingPanel();
      addVas3kTranscriptLine({ speaker: 'Alice', time: '10:00:01', text: 'First', id: 1 });
      addVas3kTranscriptLine({ speaker: 'Bob', time: '10:00:05', text: 'Second', id: 2 });

      highlightVas3kLine(1);

      const lines = document.querySelectorAll('.jitsi-transcript-line');
      expect(lines[0].classList.contains('highlighted')).toBe(true);
      expect(lines[1].classList.contains('highlighted')).toBe(false);
    });

    test('scrolls highlighted line into view', () => {
      createFloatingPanel();
      addVas3kTranscriptLine({ speaker: 'Alice', time: '10:00:01', text: 'First', id: 1 });

      const line = document.querySelector('.jitsi-transcript-line');
      const scrollIntoViewSpy = jest.spyOn(line, 'scrollIntoView');

      highlightVas3kLine(1);

      expect(scrollIntoViewSpy).toHaveBeenCalledWith({ behavior: 'smooth', block: 'center' });
    });

    test('removes highlight after timeout', async () => {
      jest.useFakeTimers();
      createFloatingPanel();
      addVas3kTranscriptLine({ speaker: 'Alice', time: '10:00:01', text: 'First', id: 1 });

      highlightVas3kLine(1);

      const line = document.querySelector('.jitsi-transcript-line');
      expect(line.classList.contains('highlighted')).toBe(true);

      jest.advanceTimersByTime(2000);

      expect(line.classList.contains('highlighted')).toBe(false);
      jest.useRealTimers();
    });
  });
});

describe('Transcript Panel Layout', () => {
  let createFloatingPanel;

  beforeAll(async () => {
    const module = await import('./ui-vas3k.js');
    createFloatingPanel = module.createFloatingPanel;
  });

  beforeEach(() => {
    document.body.innerHTML = '';
  });

  test('header controls have flex-wrap for button overflow', () => {
    createFloatingPanel();

    const controls = document.querySelector('.jitsi-transcript-header-controls');
    const style = window.getComputedStyle(controls);

    expect(style.flexWrap).toBe('wrap');
  });

  test('all header buttons are present', () => {
    createFloatingPanel();

    const controls = document.querySelector('.jitsi-transcript-header-controls');
    const buttons = controls.querySelectorAll('button');

    // Should have: Copy, Follow, ↑, ↓, −, ×
    expect(buttons.length).toBe(6);
  });

  test('header buttons have compact styling', () => {
    createFloatingPanel();

    const button = document.querySelector('.jitsi-transcript-header button');
    const style = window.getComputedStyle(button);

    // Font should be small
    expect(parseInt(style.fontSize)).toBeLessThanOrEqual(12);
  });
});
