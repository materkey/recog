/**
 * Which audio tracks of a KTalk participant the adapter listens to.
 *
 * @jest-environment jsdom
 */

import { jest } from '@jest/globals';

const pcm = {
  pcmAttachParticipant: jest.fn(),
  pcmDetachParticipant: jest.fn(),
  pcmAttachAllFromConf: jest.fn(),
  pcmTransportDispose: jest.fn(),
  pcmUpdateParticipantName: jest.fn(),
};
const analyzer = {
  attachParticipant: jest.fn(),
  detachParticipant: jest.fn(),
  attachAllFromConf: jest.fn(),
  dispose: jest.fn(),
  isSpeech: jest.fn(),
  getFlatness: jest.fn(),
};

jest.unstable_mockModule('../pcm_transport.js', () => pcm);
jest.unstable_mockModule('../audio-analyzer.js', () => analyzer);

const { KTalkAdapter } = await import('./ktalk.js');

function makeTrack(fields) {
  return {
    getType: () => 'audio',
    getParticipantId: () => 'pid-a',
    stream: {},
    ...fields,
  };
}

function subscribedHandlers() {
  const handlers = new Map();
  const adapter = new KTalkAdapter();
  adapter.getConference = () => ({
    jitsiConf: { on: (event, handler) => handlers.set(event, handler), off: () => {} },
  });
  adapter.subscribeToSpeakerChanges(() => {});
  return handlers;
}

beforeEach(() => {
  jest.clearAllMocks();
});

test('microphone track - added - is listened to', () => {
  const track = makeTrack({ audioType: 'microphone' });

  subscribedHandlers().get('conference.trackAdded')(track);

  expect(pcm.pcmAttachParticipant).toHaveBeenCalledTimes(1);
  expect(analyzer.attachParticipant).toHaveBeenCalledTimes(1);
});

test('screen share track - added - does not replace the microphone', () => {
  const track = makeTrack({ audioType: 'screenShare' });

  subscribedHandlers().get('conference.trackAdded')(track);

  expect(pcm.pcmAttachParticipant).not.toHaveBeenCalled();
  expect(analyzer.attachParticipant).not.toHaveBeenCalled();
});

test('screen share track without audioType - added - is told by its source name', () => {
  const track = makeTrack({ getSourceName: () => 'abc-a1' });

  subscribedHandlers().get('conference.trackAdded')(track);

  expect(pcm.pcmAttachParticipant).not.toHaveBeenCalled();
});

test('microphone track without audioType - added - is told by its source name', () => {
  const track = makeTrack({ getSourceName: () => 'abc-a0' });

  subscribedHandlers().get('conference.trackAdded')(track);

  expect(pcm.pcmAttachParticipant).toHaveBeenCalledTimes(1);
});

test('screen share track - removed - keeps the participant attached', () => {
  const track = makeTrack({ audioType: 'screenShare' });

  subscribedHandlers().get('conference.trackRemoved')(track);

  expect(pcm.pcmDetachParticipant).not.toHaveBeenCalled();
  expect(analyzer.detachParticipant).not.toHaveBeenCalled();
});

test('microphone track - removed - detaches the participant', () => {
  const track = makeTrack({ audioType: 'microphone' });

  subscribedHandlers().get('conference.trackRemoved')(track);

  expect(pcm.pcmDetachParticipant).toHaveBeenCalledWith('pid-a');
  expect(analyzer.detachParticipant).toHaveBeenCalledWith('pid-a');
});
