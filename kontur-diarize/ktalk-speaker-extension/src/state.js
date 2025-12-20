export const constants = {
  MAX_HISTORY: 100,
  SPEAKING_THRESHOLD_MS: 500,
  MAX_TRANSCRIPT_LINES: 100,
  WS_URL: 'ws://localhost:8765'
};

export const state = {
  transcriptTabAdded: false,
  transcriptContentElement: null,
  originalChatContent: null,
  isTranscriptActive: false,

  speakerHistory: [],
  currentSpeaker: null,
  speakingStartTime: null,

  transcriptLines: [],

  wsConnection: null,
  wsReconnectTimer: null,
  wsConnected: false,

  conferenceReady: false,
  conferenceReadyTimeout: null,

  currentMeetingUrl: null,
  meetingCheckInterval: null,

  summaryModalElement: null,
  currentSummaryText: '',
  summaryRequestPending: false,
  currentSummaryType: 'catchup',

  debugPanelElement: null,
  debugVisible: false,
  lastParticipants: [],
  lastActiveSpeaker: null,
  autoScrollEnabled: true
};
