# BarkLog

Анализ записей с диктофона, пока собака дома одна: сколько лаяла, скулила,
выла — в процентах, по времени и по эпизодам. Ядро на Rust, Android-приложение,
затем iOS. Вся обработка на устройстве.

- План проекта: [docs/PLAN.md](docs/PLAN.md)
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
| `--dog-min 0.15` | порог уверенности «это собака», 0–1 |
| `--merge-gap 3` | паузы короче N секунд не разрывают эпизод |
| `--clips 20` | сколько фрагментов сохранить, `0` — не сохранять |
| `--no-gate` | не отсеивать тихие окна (медленнее, для сверки) |
| `--windows` | сохранить в JSON результат по каждому окну 0,48 с |

`barklog inspect запись.m4a` показывает топ классов AudioSet по каждому окну —
удобно понять, за что модель принимает конкретный звук.

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

## Модель

[YAMNet](https://github.com/tensorflow/models/tree/master/research/audioset/yamnet)
(Apache 2.0), веса сконвертированы скриптом `tools/convert_yamnet.py`.
Сеть реализована на Rust без TensorFlow/ONNX, поэтому одинаково собирается
под Android, iOS и десктоп.
