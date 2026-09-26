import { readdirSync, readFileSync } from 'node:fs';
import { jest } from '@jest/globals';

// The policy is created once per module instance, so each case loads a fresh
// copy of utils.js after installing its trustedTypes stub.
async function loadToTrustedHtml() {
  let fn;
  await jest.isolateModulesAsync(async () => {
    ({ toTrustedHtml: fn } = await import('./utils.js'));
  });
  return fn;
}

function makeFakeTrustedTypes() {
  return {
    createPolicy: jest.fn((name, rules) => ({
      createHTML: (s) => ({ trusted: rules.createHTML(s), policy: name }),
    })),
  };
}

describe('toTrustedHtml', () => {
  afterEach(() => {
    delete globalThis.trustedTypes;
    jest.restoreAllMocks();
  });

  test('no trustedTypes - wrapping html - returns the raw string', async () => {
    delete globalThis.trustedTypes;
    const toTrustedHtml = await loadToTrustedHtml();

    expect(toTrustedHtml('<b>x</b>')).toBe('<b>x</b>');
  });

  test('trustedTypes available - wrapping html twice - creates one recog policy and returns trusted values', async () => {
    const tt = makeFakeTrustedTypes();
    globalThis.trustedTypes = tt;
    const toTrustedHtml = await loadToTrustedHtml();

    const first = toTrustedHtml('<b>x</b>');
    const second = toTrustedHtml('<i>y</i>');

    expect(first).toEqual({ trusted: '<b>x</b>', policy: 'recog' });
    expect(second).toEqual({ trusted: '<i>y</i>', policy: 'recog' });
    expect(tt.createPolicy).toHaveBeenCalledTimes(1);
    expect(tt.createPolicy).toHaveBeenCalledWith('recog', expect.any(Object));
  });

  test('policy creation throws - wrapping html twice - falls back to the raw string and warns once', async () => {
    const warn = jest.spyOn(console, 'warn').mockImplementation(() => {});
    const tt = {
      createPolicy: jest.fn(() => {
        throw new TypeError('Policy "recog" disallowed.');
      }),
    };
    globalThis.trustedTypes = tt;
    const toTrustedHtml = await loadToTrustedHtml();

    expect(toTrustedHtml('<b>x</b>')).toBe('<b>x</b>');
    expect(toTrustedHtml('<i>y</i>')).toBe('<i>y</i>');
    expect(tt.createPolicy).toHaveBeenCalledTimes(1);
    expect(warn).toHaveBeenCalledTimes(1);
  });

  test('trusted value from the policy - assigned to innerHTML - renders the markup', async () => {
    globalThis.trustedTypes = {
      createPolicy: (name, rules) => ({
        createHTML: (s) => ({ toString: () => rules.createHTML(s) }),
      }),
    };
    const toTrustedHtml = await loadToTrustedHtml();
    const el = document.createElement('div');

    el.innerHTML = toTrustedHtml('<span class="a">hi</span>');

    expect(el.querySelector('.a').textContent).toBe('hi');
  });
});

describe('Trusted Types coverage', () => {
  // jsdom does not enforce Trusted Types; on Meet a plain-string assignment
  // throws, so every HTML sink in the extension sources must be wrapped.
  function sourceFiles(dir) {
    return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
      const url = new URL(entry.name + (entry.isDirectory() ? '/' : ''), dir);
      if (entry.isDirectory()) return sourceFiles(url);
      return entry.name.endsWith('.js') && !entry.name.endsWith('.test.js') ? [url] : [];
    });
  }

  test('extension sources - HTML sink assignments - all go through toTrustedHtml', () => {
    const unwrapped = [];
    for (const file of sourceFiles(new URL('./', import.meta.url))) {
      readFileSync(file, 'utf8').split('\n').forEach((line, i) => {
        if (/\.(innerHTML|outerHTML)\s*=(?!=)(?!\s*toTrustedHtml\()|insertAdjacentHTML\(/.test(line)) {
          unwrapped.push(`${file.pathname.split('/src/')[1]}:${i + 1}`);
        }
      });
    }

    expect(unwrapped).toEqual([]);
  });
});
