// react-native-tailpocket needs io.jingtao.tailpocket:tailpocket-ui, which is not on
// Maven Central: it ships as tailpocket-android-m2.zip in the TailPocket release.
// scripts/fetch-vendor.sh mobile unpacks it to vendor/tailpocket/pkg/m2; this adds
// that directory to the Android project's repositories. The absolute path is
// resolved at prebuild time, on the machine that builds.
const path = require("path");
const fs = require("fs");
const { withProjectBuildGradle } = require("expo/config-plugins");

const MARK = "// tether: tailpocket m2";

module.exports = function withTailPocketMaven(config) {
  return withProjectBuildGradle(config, cfg => {
    const m2 = path.resolve(cfg.modRequest.projectRoot, "../../vendor/tailpocket/pkg/m2");
    if (!fs.existsSync(m2)) {
      console.warn(`with-tailpocket-maven: ${m2} is missing; run scripts/fetch-vendor.sh mobile`);
    }
    const repo = `        maven { url = uri(${JSON.stringify(m2.split(path.sep).join("/"))}) } ${MARK}`;
    let src = cfg.modResults.contents.split("\n").filter(l => !l.includes(MARK)).join("\n");
    const anchor = /allprojects\s*\{\s*repositories\s*\{/;
    if (!anchor.test(src)) throw new Error("with-tailpocket-maven: no allprojects.repositories block in android/build.gradle");
    cfg.modResults.contents = src.replace(anchor, m => `${m}\n${repo}`);
    return cfg;
  });
};
