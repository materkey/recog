import { bootstrapRuntime } from './runtime.js';

afterEach(() => { delete window.__recogDesktopRuntime; });

test('two concurrent injections - initializing once - create one runtime', () => {
  const initialize = jest.fn(() => bootstrapRuntime('1.0.0', jest.fn()));
  const first = bootstrapRuntime('1.0.0', initialize);
  const second = bootstrapRuntime('1.0.0', initialize);
  expect(first).toBe(second);
  expect(first).toEqual({ version: '1.0.0', state: 'ready' });
  expect(initialize).toHaveBeenCalledTimes(1);
});

test('failed initialization - injecting again - waits for page reload', () => {
  const initialize = jest.fn(() => { throw new Error('partial init'); });
  const error = jest.spyOn(console, 'error').mockImplementation(() => {});
  try {
    expect(bootstrapRuntime('1.0.0', initialize).state).toBe('failed');
    bootstrapRuntime('1.0.0', initialize);
    expect(initialize).toHaveBeenCalledTimes(1);
  } finally { error.mockRestore(); }
});

test('document without body - DOM ready - starts exactly once', () => {
  const body = jest.spyOn(document, 'body', 'get').mockReturnValue(null);
  const initialize = jest.fn();
  try {
    const runtime = bootstrapRuntime('1.0.0', initialize);
    bootstrapRuntime('1.0.0', initialize);
    expect(initialize).not.toHaveBeenCalled();
    document.dispatchEvent(new Event('DOMContentLoaded'));
    document.dispatchEvent(new Event('DOMContentLoaded'));
    expect(initialize).toHaveBeenCalledTimes(1);
    expect(runtime.state).toBe('ready');
  } finally { body.mockRestore(); }
});
