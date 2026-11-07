import javax.inject.Inject

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
}

// Firebase (удалённый просмотр): конфиг своего проекта кладётся в app/google-services.json.
// Без него приложение собирается и работает, просто без удалённого просмотра.
if (file("google-services.json").exists()) {
    apply(plugin = "com.google.gms.google-services")
}

// Корень репозитория с Rust-workspace.
val repoRoot: File = rootDir.parentFile

android {
    namespace = "app.barklog"
    compileSdk {
        version = release(37) { minorApiLevel = 2 }
    }
    ndkVersion = "30.0.16248370"
    buildToolsVersion = "36.1.0"

    defaultConfig {
        applicationId = "app.barklog"
        minSdk = 26
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
        // Для отладки с Firebase Emulator Suite: ./gradlew -Pbarklog.firebaseEmulator=10.0.2.2 ...
        buildConfigField(
            "String",
            "FIREBASE_EMULATOR_HOST",
            "\"${providers.gradleProperty("barklog.firebaseEmulator").getOrElse("")}\"",
        )
        ndk {
            // arm64 — телефоны, x86_64 — эмулятор.
            abiFilters += listOf("arm64-v8a", "x86_64")
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.getByName("debug")
        }
    }

    buildFeatures {
        compose = true
        buildConfig = true
    }

    packaging {
        // Модель и так сжата плохо, а без сжатия её можно читать прямо из APK.
        resources.excludes += "/META-INF/{AL2.0,LGPL2.1}"
    }
    androidResources {
        noCompress += "safetensors"
    }
}

// ---- Rust: libbarklog_ffi.so через cargo-ndk, Kotlin-биндинги через UniFFI, модель в assets ----

/** Собирает barklog-ffi под Android через cargo-ndk. */
abstract class CargoNdkTask : DefaultTask() {
    @get:InputFiles abstract val sources: ConfigurableFileCollection
    @get:Internal abstract val workspace: DirectoryProperty
    @get:Internal abstract val ndkDir: DirectoryProperty
    @get:Input abstract val abis: ListProperty<String>
    @get:OutputDirectory abstract val outputDir: DirectoryProperty
    @get:Inject abstract val exec: ExecOperations

    @TaskAction
    fun build() {
        val targets = abis.get().flatMap { listOf("-t", it) }
        exec.exec {
            workingDir = workspace.get().asFile
            environment("ANDROID_NDK_HOME", ndkDir.get().asFile.absolutePath)
            commandLine(
                listOf("cargo", "ndk") + targets +
                    listOf("-P", "26", "-o", outputDir.get().asFile.absolutePath, "build", "--release", "-p", "barklog-ffi"),
            )
        }
    }
}

/** Генерирует Kotlin-биндинги из собранной библиотеки (метаданные UniFFI зашиты в .so). */
abstract class UniffiBindingsTask : DefaultTask() {
    @get:InputDirectory abstract val libDir: DirectoryProperty
    @get:Internal abstract val workspace: DirectoryProperty
    @get:OutputDirectory abstract val outputDir: DirectoryProperty
    @get:Inject abstract val exec: ExecOperations

    @TaskAction
    fun generate() {
        val lib = libDir.get().file("arm64-v8a/libbarklog_ffi.so").asFile.absolutePath
        exec.exec {
            workingDir = workspace.get().asFile
            commandLine(
                "cargo", "run", "-q", "--release",
                "-p", "barklog-ffi", "--features", "bindgen", "--bin", "uniffi-bindgen", "--",
                "generate", "--library", lib, "--language", "kotlin", "--no-format",
                "--out-dir", outputDir.get().asFile.absolutePath,
            )
        }
    }
}

/** Кладёт модель YAMNet в assets. */
abstract class ModelAssetTask : DefaultTask() {
    @get:InputFile abstract val model: RegularFileProperty
    @get:OutputDirectory abstract val outputDir: DirectoryProperty

    @TaskAction
    fun copy() {
        val out = outputDir.get().asFile
        out.mkdirs()
        model.get().asFile.copyTo(out.resolve("yamnet.safetensors"), overwrite = true)
    }
}

val cargoNdk = tasks.register<CargoNdkTask>("cargoNdk") {
    workspace.set(repoRoot)
    sources.from(fileTree(repoRoot.resolve("crates")) { exclude("**/target/**") })
    sources.from(repoRoot.resolve("Cargo.toml"), repoRoot.resolve("Cargo.lock"))
    ndkDir.set(androidComponents.sdkComponents.ndkDirectory)
    abis.set(listOf("arm64-v8a", "x86_64"))
}

val uniffiBindings = tasks.register<UniffiBindingsTask>("uniffiBindings") {
    workspace.set(repoRoot)
    libDir.set(cargoNdk.flatMap { it.outputDir })
}

val modelAsset = tasks.register<ModelAssetTask>("modelAsset") {
    model.set(repoRoot.resolve("models/yamnet.safetensors"))
}

androidComponents {
    onVariants { variant ->
        variant.sources.jniLibs?.addGeneratedSourceDirectory(cargoNdk, CargoNdkTask::outputDir)
        variant.sources.kotlin?.addGeneratedSourceDirectory(uniffiBindings, UniffiBindingsTask::outputDir)
        variant.sources.assets?.addGeneratedSourceDirectory(modelAsset, ModelAssetTask::outputDir)
    }
}

dependencies {
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.activity.compose)
    implementation(libs.androidx.lifecycle.viewmodel.compose)
    implementation(libs.androidx.lifecycle.runtime.compose)
    implementation(libs.androidx.work.runtime.ktx)
    implementation(platform(libs.compose.bom))
    implementation(libs.compose.ui)
    implementation(libs.compose.foundation)
    implementation(libs.compose.material3)
    implementation(libs.compose.ui.tooling.preview)
    debugImplementation(libs.compose.ui.tooling)
    implementation("${libs.jna.get()}@aar")
    implementation(platform(libs.firebase.bom))
    implementation(libs.firebase.auth)
    implementation(libs.firebase.firestore)
    implementation(libs.firebase.messaging)
    implementation(libs.credentials)
    implementation(libs.credentials.play)
    implementation(libs.googleid)
    implementation(libs.coroutines.play)
}
