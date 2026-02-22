export const constants = {
  MAX_HISTORY: 100,
  SPEAKING_THRESHOLD_MS: 500,
  MAX_TRANSCRIPT_LINES: 100,
  WS_URL: 'ws://localhost:8766',
  FLATNESS_THRESHOLD: 0.15,
  AUDIO_FILTER_ENABLED: true
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
  contextContentElement: null,
  isContextActive: false,
  anchorRequestPending: false,
  currentAnchorData: null,
  anchorLastUpdated: 0,

  debugPanelElement: null,
  debugVisible: false,
  lastParticipants: [],
  lastActiveSpeaker: null,
  lastParticipantsMapSize: 0,
  lastJitsiParticipantsCount: 0,
  lastJitsiParticipantsMapCount: 0,
  autoScrollEnabled: true,
  participantsRefreshTimer: null,
  clockSyncTimer: null,
  activeConfRef: null,
  activeJitsiConf: null,
  audioFilterSuppressCount: 0,
  audioFilterLastFlatness: null,
  audioFilterLastReason: null,

  bioRead: {
    enabled: false,
    fixation: 40,
    tailOpacity: 60,
    minWordLength: 3,
    maxHeadRatio: 0.6,
    ignoreNumbers: true,
    panelOpen: false
  }
};
