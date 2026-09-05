import { escapeHtml } from './utils.js';

export const BIOREAD_SETTINGS_KEY = 'ktalk-bioread-settings';

export const BIOREAD_LIMITS = {
  fixation: { min: 10, max: 60, step: 5, default: 40 },
  tailOpacity: { min: 35, max: 90, step: 5, default: 60 },
  minWordLength: 3,
  maxHeadRatio: 0.6,
  ignoreNumbers: true
};

export function normalizeBioReadSettings(input = {}) {
  const fixation = clampNumber(
    parseInt(input.fixation, 10),
    BIOREAD_LIMITS.fixation.default,
    BIOREAD_LIMITS.fixation.min,
    BIOREAD_LIMITS.fixation.max
  );
  const tailOpacity = clampNumber(
    parseInt(input.tailOpacity, 10),
    BIOREAD_LIMITS.tailOpacity.default,
    BIOREAD_LIMITS.tailOpacity.min,
    BIOREAD_LIMITS.tailOpacity.max
  );

  return {
    enabled: Boolean(input.enabled),
    fixation,
    tailOpacity,
    minWordLength: BIOREAD_LIMITS.minWordLength,
    maxHeadRatio: BIOREAD_LIMITS.maxHeadRatio,
    ignoreNumbers: input.ignoreNumbers !== undefined ? Boolean(input.ignoreNumbers) : BIOREAD_LIMITS.ignoreNumbers
  };
}

export function formatBioReadText(text, settings) {
  const safeText = typeof text === 'string' ? text : String(text ?? '');
  if (!settings || !settings.enabled) {
    return safeText;
  }

  const tokens = tokenize(safeText);
  return tokens.map(token => formatToken(token, settings)).join('');
}

function tokenize(text) {
  if (!text) return [''];
  return text.match(/[\p{L}\p{M}\p{N}]+|[^\p{L}\p{M}\p{N}]+/gu) || [text];
}

function formatToken(token, settings) {
  if (!token) return '';

  if (isWordToken(token, settings)) {
    if (settings.ignoreNumbers && isNumericToken(token)) {
      return escapeHtml(token);
    }

    if (token.length < settings.minWordLength) {
      return escapeHtml(token);
    }

    const headLen = getHeadLength(token.length, settings.fixation, settings.maxHeadRatio);
    if (headLen <= 0 || headLen >= token.length) {
      return escapeHtml(token);
    }

    const head = token.slice(0, headLen);
    const tail = token.slice(headLen);
    return `<span class="bio-word"><span class="bio-head">${escapeHtml(head)}</span><span class="bio-tail">${escapeHtml(tail)}</span></span>`;
  }

  return escapeHtml(token);
}

function isWordToken(token, settings) {
  if (/[\p{L}\p{M}]/u.test(token)) return true;
  if (!settings.ignoreNumbers && /[\p{N}]/u.test(token)) return true;
  return false;
}

function isNumericToken(token) {
  return /^[\p{N}]+$/u.test(token);
}

function getHeadLength(wordLength, fixationPercent, maxHeadRatio) {
  const ratio = fixationPercent / 100;
  const raw = Math.ceil(wordLength * ratio);
  const max = Math.max(1, Math.floor(wordLength * maxHeadRatio));
  return clamp(raw, 1, max);
}

function clampNumber(value, fallback, min, max) {
  if (!Number.isFinite(value)) return fallback;
  return clamp(value, min, max);
}

function clamp(value, min, max) {
  return Math.min(Math.max(value, min), max);
}
