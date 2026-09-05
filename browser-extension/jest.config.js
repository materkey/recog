export default {
  testEnvironment: 'jsdom',
  transform: {
    '^.+\\.js$': 'babel-jest',
  },
  moduleFileExtensions: ['js'],
  testMatch: ['**/src/**/*.test.js'],
  injectGlobals: true,
  setupFilesAfterEnv: ['<rootDir>/src/test-setup.js'],
};
