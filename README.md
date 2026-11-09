# BarkLog

Анализ записей с диктофона, пока собака дома одна: сколько лаяла, скулила,
выла — в процентах, по времени и по эпизодам. Ядро на Rust, Android-приложение,
затем iOS. Вся обработка на устройстве.

- Дизайн: [docs/DESIGN.md](docs/DESIGN.md)

## Запуск на компьютере

Нужен Rust и Git LFS (в LFS лежит модель `models/yamnet.safetensors`).

```sh
git lfs pull
cargo build --release
./target/release/barklog analyze запись.m4a
```

Рядом с файлом появится папка `запись_barklog/` с `report.html`,
`report.json` и фрагментами самых длинных эпизодов в `clips/`.

Полезные параметры:

| Параметр | Что делает |
|----------|------------|
| `--start "2026-11-07 09:30"` | время начала записи (иначе из имени файла или даты изменения) |
| `--dog-min 0.15` | порог уверенности «это собака», 0,05–0,9 |
| `--merge-gap 3` | паузы короче N секунд не разрывают эпизод |
| `--min-episode 1` | эпизоды короче N секунд не считаются (по умолчанию все) |
| `--clips 20` | сколько фрагментов сохранить, `0` — не сохранять |
| `--no-gate` | не отсеивать тихие окна (медленнее, для сверки) |
| `--windows` | сохранить в JSON результат по каждому окну 0,48 с |

`barklog inspect запись.m4a` показывает топ классов AudioSet по каждому окну —
удобно понять, за что модель принимает конкретный звук.

## CI

`.github/workflows/ci.yml` на каждый пуш в `main` и pull request:
`cargo fmt --check`, `clippy -D warnings`, тесты (с моделью из LFS — тесты сети
не пропускаются) и сборка debug-APK, который можно скачать из артефактов запуска.
Версия Rust в CI закреплена (`RUST_TOOLCHAIN`), её поднимают вместе с локальной.

## Android

Нужны Android SDK (платформа 36.1), NDK 30, JDK 21 (подойдёт `jbr` из Android Studio)
и cargo-ndk:

```sh
rustup target add aarch64-linux-android x86_64-linux-android
cargo install cargo-ndk
cd android
./gradlew assembleDebug        # APK: app/build/outputs/apk/debug/
./gradlew installDebug         # на подключённый телефон или эмулятор
```

Gradle сам собирает `libbarklog_ffi.so` через cargo-ndk (arm64-v8a и x86_64),
генерирует Kotlin-биндинги UniFFI и кладёт модель в assets. Проект можно открыть
в Android Studio — папка `android/`.

## Удалённый просмотр (Firebase)

Чтобы со второго телефона видеть идущую запись, нужен свой проект Firebase:

1. Создать проект, добавить Android-приложение `app.barklog` с SHA-1 ключа подписи.
2. Authentication → Sign-in method → Google; Firestore Database → создать (production).
3. Скачать `google-services.json` в `android/app/` (в git не попадает).
4. Firestore → Rules → вставить содержимое `firebase/firestore.rules`
   (или `firebase deploy --only firestore:rules` из папки `firebase/`).
5. Пуши «собака лает 10 минут» — `firebase deploy --only functions` (нужен тариф Blaze).

Без `google-services.json` приложение работает, просто без удалённого просмотра.

Проверка без настоящего проекта — локальный Firebase Emulator Suite:

```sh
cd firebase && firebase emulators:start --only auth,firestore --project demo-barklog
cd android && ./gradlew installDebug -Pbarklog.firebaseEmulator=10.0.2.2
```

## Модель

[YAMNet](https://github.com/tensorflow/models/tree/master/research/audioset/yamnet)
(Apache 2.0), веса сконвертированы скриптом `tools/convert_yamnet.py`.
Сеть реализована на Rust без TensorFlow/ONNX, поэтому одинаково собирается
под Android, iOS и десктоп.
