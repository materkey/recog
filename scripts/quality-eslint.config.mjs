// ESLint configuration of the quality gate (docs/quality.md). The tools are
// pinned in deploy/node and resolved from there, so this file stays beside the
// other quality configurations instead of next to an installation.
import { createRequire } from 'node:module';

const require = createRequire(new URL('../deploy/node/package.json', import.meta.url));
const js = require('@eslint/js');
const globals = require('globals');

// `_` names a deliberately ignored binding, and a caught error may go unused.
const unused = {
  args: 'after-used',
  caughtErrors: 'none',
  argsIgnorePattern: '^_',
  varsIgnorePattern: '^_',
  destructuredArrayIgnorePattern: '^_',
};

export default [
  {
    ignores: ['**/node_modules/**', 'browser-extension/dist/**', 'browser-extension/injected.js'],
  },
  {
    files: ['**/*.js', '**/*.cjs', '**/*.mjs'],
    ...js.configs.recommended,
    rules: { ...js.configs.recommended.rules, 'no-unused-vars': ['error', unused] },
  },
  {
    // Bundled by webpack into the conference page, the extension's own pages
    // and the audio worklet.
    files: ['browser-extension/src/**/*.js'],
    languageOptions: {
      sourceType: 'module',
      globals: {
        ...globals.browser,
        ...globals.webextensions,
        // webpack.DefinePlugin constants.
        __RECOG_CONFERENCE_HOSTS__: 'readonly',
        __RECOG_RUNTIME_VERSION__: 'readonly',
      },
    },
  },
  {
    files: ['browser-extension/src/audio-worklet/**/*.js'],
    languageOptions: { globals: globals.audioWorklet },
  },
  {
    files: ['browser-extension/src/**/*.test.js', 'browser-extension/src/test-setup.js'],
    languageOptions: { globals: { ...globals.jest, ...globals.node } },
  },
  {
    files: ['browser-extension/content.js', 'browser-extension/popup.js'],
    languageOptions: {
      sourceType: 'script',
      globals: { ...globals.browser, ...globals.webextensions },
    },
  },
  {
    files: ['browser-extension/*.config.js', 'browser-extension/scripts/**/*.js'],
    languageOptions: { sourceType: 'module', globals: globals.node },
  },
  {
    // Classic scripts of the web UI, loaded in order by index.html: their
    // top-level functions are globals called from markup and later scripts.
    files: ['crates/recog-web/static/**/*.js', 'deploy/landing/**/*.js', 'tests/e2e/**/*.js'],
    languageOptions: {
      sourceType: 'script',
      globals: globals.browser,
    },
    rules: { 'no-unused-vars': ['error', { ...unused, vars: 'local' }] },
  },
  {
    files: ['crates/recog-web/static/app.js'],
    languageOptions: { globals: { DatePicker: 'readonly' } },
  },
  {
    // Evaluated inside the KTalk desktop application's main process, which
    // calls its top-level function.
    files: ['crates/recog-desktop/bootstrap.js'],
    languageOptions: { sourceType: 'script', globals: globals.node },
    rules: { 'no-unused-vars': ['error', { ...unused, vars: 'local' }] },
  },
  {
    files: ['scripts/**/*.mjs'],
    languageOptions: { globals: globals.node },
  },
  {
    files: ['**/*.cjs'],
    languageOptions: { sourceType: 'commonjs', globals: globals.node },
  },
];
