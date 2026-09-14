import { installFocusGuard } from './focus_guard.js';
import { init } from './init.js';
import { bootstrapRuntime } from './runtime.js';

bootstrapRuntime(__RECOG_RUNTIME_VERSION__, () => {
  installFocusGuard();
  init();
});
