export const constants = {
  MAX_HISTORY: 100,
  SPEAKING_THRESHOLD_MS: 500,
  MAX_TRANSCRIPT_LINES: 100,
  WS_URL: 'ws://localhost:8766',
  FLATNESS_THRESHOLD: 0.15,
  AUDIO_FILTER_ENABLED: true,
  PCM_REBIND_ENABLED: true,
  PCM_WEBRTC_DIAGNOSTICS_ENABLED: true,
  PCM_STATS_INTERVAL_MS: 1000,
  PCM_STATS_SEND_INTERVAL_MS: 2000,
  PCM_REBIND_COOLDOWN_MS: 4000,
  AI_LAST_VIEW_KEY: 'ktalk-ai-last-view'
};

export const state = {
  transcriptContentElement: null,
  originalChatContent: null,

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
  currentMeetingId: null,
  meetingCheckInterval: null,

  summaryModalElement: null,
  currentSummaryText: '',
  summaryRequestPending: false,
  currentSummaryType: 'catchup',
  contextContentElement: null,
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
  privacyMode: localStorage.getItem('ktalk-privacy-mode') === 'true',
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
  },

  copilotMessages: [],
  copilotPending: new Map(),
  copilotProactiveMutedUntil: 0,
  copilotChannelStatus: 'unknown',
  copilotAgentState: 'offline',
  copilotLastProactiveAt: 0,
  copilotContentElement: null,
  isCopilotActive: false,
  copilotUnread: false,
  lastAiView: localStorage.getItem(constants.AI_LAST_VIEW_KEY) || 'transcript'
};
