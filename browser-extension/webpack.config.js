import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import webpack from 'webpack';
import { createManifest, loadConferenceHosts } from './scripts/conference-hosts.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const packageVersion = JSON.parse(fs.readFileSync(path.join(__dirname, 'package.json'), 'utf8')).version;
const hosts = loadConferenceHosts();
const manifest = createManifest(JSON.parse(fs.readFileSync(path.join(__dirname, 'manifest.json'), 'utf8')), hosts);
const assets = ['content.js', 'popup.html', 'popup.js', 'src/audio-worklet/downsampler.js',
  'icons/icon16.png', 'icons/icon48.png', 'icons/icon128.png'];

export default {
  mode: 'development',
  context: __dirname,
  entry: './src/index.js',
  output: {
    path: path.join(__dirname, 'dist'),
    filename: 'injected.js'
  },
  plugins: [
    new webpack.DefinePlugin({ __RECOG_CONFERENCE_HOSTS__: JSON.stringify(hosts), __RECOG_RUNTIME_VERSION__: JSON.stringify(packageVersion) }),
    {
      apply(compiler) {
        compiler.hooks.thisCompilation.tap('ExtensionAssets', compilation => {
          compilation.hooks.processAssets.tap({
            name: 'ExtensionAssets',
            stage: webpack.Compilation.PROCESS_ASSETS_STAGE_ADDITIONAL,
          }, () => {
            compilation.emitAsset('manifest.json', new webpack.sources.RawSource(JSON.stringify(manifest, null, 2) + '\n'));
            for (const asset of assets) {
              compilation.emitAsset(asset, new webpack.sources.RawSource(fs.readFileSync(path.join(__dirname, asset))));
            }
          });
        });
      },
    },
  ],
  devtool: false
};
