import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

/**
 * Where the first-launch disclaimer is materialised as a resource.
 *
 * Declared out here because two places need the same path — the `res.srcDir`
 * inside the `android` block and the task below that fills it — and a path
 * written twice is a path that drifts.
 */
val disclaimerResDir = layout.buildDirectory.dir("generated/disclaimer/res")

android {
    namespace = "dev.detour"
    compileSdk = 36

    defaultConfig {
        applicationId = "dev.detour"
        // 26 is where `VpnService` gained the pieces this app relies on and where
        // notification channels became mandatory. Going lower would mean two
        // code paths for the tunnel and the foreground service.
        minSdk = 26
        targetSdk = 36
        versionCode = 4
        versionName = "0.2.2"
    }

    // --- ABI splits ---------------------------------------------------------
    //
    // Opt-in through `-PabiSplits`, and deliberately not the default. A split
    // APK is what a release wants — the kernel is a 1.9 MB `.so` per ABI, so a
    // universal build ships a copy the device will never load — but the switch
    // cannot be unconditional: `assembleDebug` is what the device harness
    // installs, and a split debug build produces several APKs with no single
    // artifact to hand to `adb install -r`. Behind a property, the local
    // workflow stays byte-for-byte what it was.
    if (project.hasProperty("abiSplits")) {
        splits {
            abi {
                isEnable = true
                reset()
                // Only the two ABIs the kernel is built for. Listing them
                // explicitly rather than taking every ABI keeps a future
                // `armeabi-v7a` dependency from silently appearing in the
                // release as an APK whose library does not exist.
                include("arm64-v8a", "x86_64")
                // No universal APK alongside the splits: it would be the same
                // artifact the splits exist to avoid, and it invites a user to
                // download the larger file by mistake.
                isUniversalApk = false
            }
        }
    }

    // --- release signing ----------------------------------------------------
    //
    // Signing is driven entirely by the environment, so the repository never
    // holds a key. A committed keystore would be worse than an unsigned build:
    // anyone who could read the repo could then sign an update the device would
    // accept as this app, and this app installs a VPN — "who can sign a build"
    // *is* the trust boundary here, not a formality. The keystore is created
    // once, kept by the owner, and handed to CI through repository secrets.
    //
    // With the variable absent (every local build) no signing config is created
    // and `assembleRelease` still stops at an unsigned APK, which is the
    // behaviour the release block below has always documented.
    val releaseKeystorePath = System.getenv("WATT_KEYSTORE")
    signingConfigs {
        if (releaseKeystorePath != null) {
            create("release") {
                storeFile = file(releaseKeystorePath)
                storePassword = System.getenv("WATT_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("WATT_KEY_ALIAS")
                keyPassword = System.getenv("WATT_KEY_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            // Left unsignable on purpose: a release build without a keystore is a
            // deliberate stopping point, not an oversight.
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"))
            if (releaseKeystorePath != null) {
                signingConfig = signingConfigs.getByName("release")
            }
        }
    }

    buildFeatures {
        compose = true
        // BuildConfig carries the debug flag the control receiver checks, so a
        // release build cannot be driven over adb even by accident.
        buildConfig = true
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlin {
        compilerOptions {
            jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
        }
    }

    packaging {
        resources {
            excludes += "/META-INF/{AL2.0,LGPL2.1}"
        }
    }

    // The kernel is a Rust `cdylib` built by `scripts/android-build.sh`. It is
    // copied in rather than built here: Gradle has no business driving cargo,
    // and keeping the two apart means the Rust build can be run and debugged on
    // its own.
    sourceSets["main"].jniLibs.srcDirs("src/main/jniLibs")

    // --- the disclaimer the app shows on first launch -----------------------
    //
    // `DISCLAIMER.md` at the repository root is the **only** copy of that text,
    // and it is materialised here as a resource so the app can show exactly what
    // the repository publishes.
    //
    // Generated rather than checked in, for two separate reasons. A checked-in
    // copy under `src/` would be a second version of a legal document to keep in
    // step with the first, which is the same drift this project has already paid
    // for three times with values that outlived what they described. And a build
    // step that writes into the source tree dirties the working tree, which is
    // how a stray file ends up in the next `git add`.
    sourceSets["main"].res.srcDir(disclaimerResDir)
}

/**
 * Copies `DISCLAIMER.md` in as `res/raw/disclaimer.md`.
 *
 * The rename is not cosmetic: a `res/raw` name has to be a lowercase identifier,
 * so `DISCLAIMER.md` could not be a resource name at all. `R.raw.disclaimer` is
 * what the app opens.
 *
 * The `into("raw")` inside the `from` block — rather than on the task — is what
 * puts the file in a resource *type* directory. `res.srcDir` is a resource root,
 * and a file sitting directly in one is not a resource: `res/disclaimer.md` has no
 * type, so the merger either rejects it or ignores it and `R.raw.disclaimer` never
 * appears. The task's own `into` is the root; the child `into` is the `raw/`
 * inside it.
 *
 * Hooked to `preBuild` rather than to a resource-merge task by name, because every
 * merge task depends on `preBuild` transitively while AGP's own task names are an
 * implementation detail that has changed between releases.
 */
val syncDisclaimer = tasks.register<Sync>("syncDisclaimer") {
    from(rootProject.file("../DISCLAIMER.md")) {
        rename { "disclaimer.md" }
        into("raw")
    }
    into(disclaimerResDir)
}

tasks.named("preBuild") {
    dependsOn(syncDisclaimer)
}

dependencies {
    val composeBom = platform("androidx.compose:compose-bom:2025.12.01")
    implementation(composeBom)

    implementation("androidx.core:core-ktx:1.15.0")
    implementation("androidx.activity:activity-compose:1.10.1")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.8.7")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.8.7")
    implementation("androidx.navigation:navigation-compose:2.8.5")

    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-graphics")
    implementation("androidx.compose.ui:ui-tooling-preview")
    implementation("androidx.compose.foundation:foundation")

    // Version from the BOM, not pinned. The BOM resolves this to material3
    // 1.4.0. In 1.4.0 the expressive theme API is `internal`, so the app builds
    // its theme on the standard `MaterialTheme`; pinning a different version
    // would only add a value that can drift from the BOM for no benefit.
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material3:material3-window-size-class")

    // The backdrop sampler the frosted glass draws with. Compose has no built-in
    // way to read the pixels *behind* a composable — `Modifier.blur` blurs the
    // caller's own layer, not what is underneath — so before this the glass was
    // a translucent tint that only looked frosted. `backdrop` records the
    // background into a graphics layer and samples it through a `RenderEffect`.
    // Apache-2.0 and `minSdk 21`, so it does not move our 26 floor, and it pulls
    // in `io.github.kyant0:shapes`, which the lens effect reads corner radii from.
    //
    // **1.0.6, and not the latest (2.0.1). This is a compileSdk ceiling, not a
    // preference.** 2.0.1 fails `:app:checkDebugAarMetadata` with 22 issues: the
    // AAR itself declares `minCompileSdk=37`, it drags `shapes` to 1.2.1 (also 37),
    // and it drags Compose to 1.12.0 (also 37) — while this module compiles
    // against 36 and the AGP in use (8.13.2) tops out at 36. Raising compileSdk to
    // 37 would mean AGP 9.1.0 and Gradle 9, a blast radius far larger than the
    // feature. 1.0.6 is the newest release whose whole closure fits:
    // `backdrop-android` 36, `shapes-android` 1.2.0 36, Compose 1.10.3 35.
    //
    // The API cost of the downgrade is one pair of helpers. 2.0.1 adds
    // `PlatformKt.isRenderEffectSupported()` / `isRuntimeShaderSupported()`, which
    // 1.0.6 does not have; `LiquidGlass.kt` inlines them as `SDK_INT` tests. Every
    // other signature used there — `drawBackdrop`, `rememberLayerBackdrop`,
    // `layerBackdrop`, `blur`, `lens`, `vibrancy`, and all their parameter names —
    // is byte-for-byte identical between the two versions.
    implementation("io.github.kyant0:backdrop:1.0.6")

    // Not brought in by material3 transitively; the navigation bar icons need it.
    implementation("androidx.compose.material:material-icons-core:1.7.8")

    // Markdown rendering, for the first-launch disclaimer and the release notes.
    //
    // This replaces a hand-rolled flattener that deleted Markdown markers
    // character by character (`#`, `>`, `**`, backticks). That was defensible while
    // the only document was a GitHub release body; the disclaimer is a 23 KB
    // structured document, and the flattener left its markup visible — inline links
    // rendered as `[LICENSE](LICENSE)` and italic markers as `*which address*`,
    // because the whole approach could only ever delete the markers it had been
    // taught about. Rendering is the point now, not stripping.
    //
    // The `-m3` artifact carries Material 3 colours, typography and components, so
    // the output follows the app's theme instead of arriving with its own palette.
    //
    // **0.41.0, and not the latest (0.45.0). This is a compileSdk ceiling, not a
    // preference** — the same wall `backdrop` hit above. 0.42.0 and later declare
    // `minCompileSdk=37` in their AAR metadata, so `:app:checkDebugAarMetadata`
    // refuses them: this module compiles against 36 and AGP 8.13.2 tops out there.
    // 0.41.0 is the newest release that declares 36. Check the AAR's
    // `META-INF/com/android/build/gradle/aar-metadata.properties` before bumping,
    // not the version number.
    //
    // It pulls `org.jetbrains:markdown` (the parser) and does **not** move Compose:
    // with it on the classpath, `androidx.compose.ui:ui` still resolves to 1.10.3
    // from the BOM above.
    implementation("com.mikepenz:multiplatform-markdown-renderer-m3:0.41.0")

    // `material-icons-extended` is not pulled in. It was deprecated and the icons
    // this app needs are all in `material-icons-core`, which `material3` already
    // brings.
    debugImplementation("androidx.compose.ui:ui-tooling")
}
