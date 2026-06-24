const { execFileSync } = require('child_process')
const path = require('path')

exports.default = async function afterPack(context) {
  if (context.electronPlatformName !== 'darwin') return

  const appName = context.packager.appInfo.productFilename
  const appPath = path.join(context.appOutDir, `${appName}.app`)

  console.log('[ad-hoc-sign] signing:', appPath)
  execFileSync('codesign', [
    '--force', '--deep', '--sign', '-', '--timestamp=none',
    appPath,
  ], { stdio: 'inherit' })
  execFileSync('codesign', ['--verify', '--verbose', appPath], { stdio: 'inherit' })
  console.log('[ad-hoc-sign] done')
}
