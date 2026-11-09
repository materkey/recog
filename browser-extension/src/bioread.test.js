/**
 * BioRead formatting: the bold head of each word the transcript shows when
 * BioRead is on.
 *
 * @jest-environment jsdom
 */

import { formatBioReadText, normalizeBioReadSettings } from './bioread.js';

const on = (extra = {}) => normalizeBioReadSettings({ enabled: true, ...extra });

function word(head, tail) {
  return `<span class="bio-word"><span class="bio-head">${head}</span><span class="bio-tail">${tail}</span></span>`;
}

describe('formatBioReadText', () => {
  test('BioRead off - formatting a text - returns it unchanged, markup included', () => {
    expect(formatBioReadText('a <b> c', normalizeBioReadSettings({ enabled: false }))).toBe('a <b> c');
    expect(formatBioReadText('a <b> c', null)).toBe('a <b> c');
  });

  test('BioRead off - formatting null - returns an empty string', () => {
    expect(formatBioReadText(null, null)).toBe('');
  });

  test('BioRead on with defaults - a six-letter word - bolds the first 40 percent rounded up', () => {
    expect(formatBioReadText('привет', on())).toBe(word('при', 'вет'));
  });

  test('BioRead on - a three-letter word - caps the head at 60 percent of the word', () => {
    expect(formatBioReadText('кот', on())).toBe(word('к', 'от'));
  });

  test('BioRead on - a word shorter than three letters - leaves it plain', () => {
    expect(formatBioReadText('да', on())).toBe('да');
  });

  test('BioRead on - punctuation and markup between words - are escaped, not bolded', () => {
    expect(formatBioReadText('ок, <да>', on())).toBe('ок, &lt;да&gt;');
  });

  test('BioRead on ignoring numbers - a number - stays plain', () => {
    expect(formatBioReadText('12345', on())).toBe('12345');
  });

  test('BioRead on not ignoring numbers - a number - gets a bold head', () => {
    expect(formatBioReadText('12345', on({ ignoreNumbers: false }))).toBe(word('12', '345'));
  });

  test('BioRead on not ignoring numbers - a short number - stays plain', () => {
    expect(formatBioReadText('12', on({ ignoreNumbers: false }))).toBe('12');
  });

  test('BioRead on - letters mixed with digits - count as a word', () => {
    expect(formatBioReadText('abc12', on())).toBe(word('ab', 'c12'));
  });

  test('BioRead on - an empty text - returns an empty string', () => {
    expect(formatBioReadText('', on())).toBe('');
  });

  test('BioRead on with a higher fixation - a long word - bolds more of it', () => {
    expect(formatBioReadText('абвгдежзик', on({ fixation: 60 }))).toBe(word('абвгде', 'жзик'));
  });
});

describe('normalizeBioReadSettings', () => {
  test('out-of-range and garbage values - normalized - clamp to the limits or the defaults', () => {
    expect(normalizeBioReadSettings({ fixation: 500, tailOpacity: 'x' })).toEqual({
      enabled: false,
      fixation: 60,
      tailOpacity: 60,
      minWordLength: 3,
      maxHeadRatio: 0.6,
      ignoreNumbers: true,
    });
  });
});
