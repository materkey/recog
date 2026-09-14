/** @jest-environment-options {"url": "https://avito.ktalk.ru/meeting/abc?settings=open#panel"} */
import { KTalkAdapter } from './ktalk.js';

test('settings query and fragment - getting KTalk meeting URL - keeps room identity', () => {
  expect(new KTalkAdapter().getMeetingUrl()).toBe('https://avito.ktalk.ru/meeting/abc');
});
