//! Личная модель: маленький классификатор поверх эмбеддингов YAMNet, обученный
//! на эпизодах, которые хозяин отметил сам («это скулёж», «это не моя собака»).
//!
//! YAMNet знает собак вообще и путает тихий скулёж с плачем, а вой — с чем угодно.
//! Пары десятков отмеченных фрагментов хватает, чтобы научиться звукам конкретной собаки.

use serde::{Deserialize, Serialize};

use crate::SoundClass;
use barklog_ml::EMBEDDING_SIZE;

/// Что пользователь сказал об эпизоде.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Label {
    Bark,
    Whine,
    Howl,
    Growl,
    /// Не собака: телевизор, улица, бытовые звуки.
    NotDog,
    /// Чужая собака — соседская, с улицы. Не голос своей собаки, но звучит почти
    /// так же, поэтому для личной модели это отдельный класс, а не «не собака».
    ForeignDog,
}

impl Label {
    /// Порядок — это номера классов в сохранённых моделях: новые только в конец.
    pub const ALL: [Label; 6] = [
        Label::Bark,
        Label::Whine,
        Label::Howl,
        Label::Growl,
        Label::NotDog,
        Label::ForeignDog,
    ];

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn class(self) -> SoundClass {
        match self {
            Label::Bark => SoundClass::Bark,
            Label::Whine => SoundClass::Whine,
            Label::Howl => SoundClass::Howl,
            Label::Growl => SoundClass::Growl,
            Label::NotDog | Label::ForeignDog => SoundClass::Other,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Label::Bark => "bark",
            Label::Whine => "whine",
            Label::Howl => "howl",
            Label::Growl => "growl",
            Label::NotDog => "not_dog",
            Label::ForeignDog => "foreign_dog",
        }
    }

    pub fn from_key(s: &str) -> Option<Self> {
        Label::ALL.into_iter().find(|l| l.key() == s)
    }
}

const CLASSES: usize = Label::ALL.len();
/// Минимум для обучения: столько окон (по 0,48 с) и хотя бы два разных ответа.
pub const MIN_SAMPLES: usize = 20;
/// Больше этого на класс не берём — обучение должно занимать секунды и на телефоне.
const MAX_PER_CLASS: usize = 800;
const EPOCHS: usize = 250;
const LEARNING_RATE: f32 = 0.05;
const L2: f32 = 1e-3;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum TrainError {
    #[error("мало отмеченных фрагментов: {have} из {need}")]
    TooFewSamples { have: usize, need: usize },
    #[error("нужны хотя бы два разных ответа — например, «лай» и «не собака»")]
    SingleClass,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonalModel {
    pub version: u32,
    mean: Vec<f32>,
    inv_std: Vec<f32>,
    /// `[CLASSES][EMBEDDING_SIZE]`.
    weights: Vec<f32>,
    bias: Vec<f32>,
    /// Классы, на которых модель вообще училась; остальные она не предсказывает.
    present: Vec<bool>,
    /// Сколько окон каждого класса было в обучении.
    pub samples: Vec<u32>,
    /// Точность на отложенной выборке (если данных хватило, чтобы её отложить).
    pub accuracy: Option<f32>,
}

impl PersonalModel {
    pub const VERSION: u32 = 1;

    pub fn total_samples(&self) -> u32 {
        self.samples.iter().sum()
    }

    /// Вероятности классов в порядке [`Label::ALL`]. У моделей, обученных до появления
    /// новых классов, вектор короче — они просто не предсказывают новые классы.
    pub fn probs(&self, embedding: &[f32]) -> Vec<f32> {
        let x: Vec<f32> = embedding
            .iter()
            .zip(&self.mean)
            .zip(&self.inv_std)
            .map(|((v, m), s)| (v - m) * s)
            .collect();
        softmax(&logits(&self.weights, &self.bias, &self.present, &x))
    }

    /// Решение по окну: класс и уверенность.
    pub fn decide(&self, embedding: &[f32]) -> (SoundClass, f32) {
        let (label, prob) = self.decide_label(embedding);
        (label.class(), prob)
    }

    /// Решение по окну в терминах ответов: отличает «чужую собаку» от «не собаки».
    pub fn decide_label(&self, embedding: &[f32]) -> (Label, f32) {
        let p = self.probs(embedding);
        let (best, prob) = p
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, &v)| (i, v))
            .expect("non-empty");
        (Label::ALL[best], prob)
    }
}

/// Обучает модель на окнах с ответами пользователя.
pub fn train(samples: &[(Vec<f32>, Label)]) -> Result<PersonalModel, TrainError> {
    if samples.len() < MIN_SAMPLES {
        return Err(TrainError::TooFewSamples {
            have: samples.len(),
            need: MIN_SAMPLES,
        });
    }
    // Не больше MAX_PER_CLASS на класс, равномерно по выборке.
    let mut picked: Vec<&(Vec<f32>, Label)> = Vec::new();
    for label in Label::ALL {
        let of: Vec<_> = samples.iter().filter(|(_, l)| *l == label).collect();
        let step = of.len().div_ceil(MAX_PER_CLASS).max(1);
        picked.extend(of.into_iter().step_by(step));
    }
    let counts = count(&picked);
    if counts.iter().filter(|&&c| c > 0).count() < 2 {
        return Err(TrainError::SingleClass);
    }

    // Точность — на каждом пятом окне, если данных достаточно.
    let accuracy = (picked.len() >= 2 * MIN_SAMPLES).then(|| {
        let (test, train_part): (Vec<_>, Vec<_>) =
            picked.iter().enumerate().partition(|(i, _)| i % 5 == 0);
        let train_part: Vec<_> = train_part.into_iter().map(|(_, s)| *s).collect();
        if count(&train_part).iter().filter(|&&c| c > 0).count() < 2 {
            return None;
        }
        let m = fit(&train_part);
        let right = test
            .iter()
            .filter(|(_, (x, l))| m.decide(x).0 == l.class())
            .count();
        Some(right as f32 / test.len().max(1) as f32)
    });

    let mut model = fit(&picked);
    model.accuracy = accuracy.flatten();
    Ok(model)
}

fn count(samples: &[&(Vec<f32>, Label)]) -> [u32; CLASSES] {
    let mut c = [0u32; CLASSES];
    for (_, l) in samples {
        c[l.index()] += 1;
    }
    c
}

/// Softmax-регрессия: нормализация признаков, веса классов обратно частоте,
/// градиентный спуск с Adam и L2.
fn fit(samples: &[&(Vec<f32>, Label)]) -> PersonalModel {
    let n = samples.len();
    let d = EMBEDDING_SIZE;
    let counts = count(samples);
    let present: Vec<bool> = counts.iter().map(|&c| c > 0).collect();

    let mut mean = vec![0f32; d];
    for (x, _) in samples {
        mean.iter_mut().zip(x).for_each(|(m, v)| *m += v / n as f32);
    }
    let mut var = vec![0f32; d];
    for (x, _) in samples {
        var.iter_mut()
            .zip(x)
            .zip(&mean)
            .for_each(|((s, v), m)| *s += (v - m).powi(2) / n as f32);
    }
    let inv_std: Vec<f32> = var.iter().map(|v| 1.0 / v.sqrt().max(1e-3)).collect();
    let xs: Vec<Vec<f32>> = samples
        .iter()
        .map(|(x, _)| {
            x.iter()
                .zip(&mean)
                .zip(&inv_std)
                .map(|((v, m), s)| (v - m) * s)
                .collect()
        })
        .collect();

    // Редкие классы весят больше, чтобы модель не отвечала всегда «лай».
    let present_classes = present.iter().filter(|&&p| p).count() as f32;
    let class_weight: Vec<f32> = counts
        .iter()
        .map(|&c| {
            if c == 0 {
                0.0
            } else {
                n as f32 / (present_classes * c as f32)
            }
        })
        .collect();

    let mut w = vec![0f32; CLASSES * d];
    let mut b = vec![0f32; CLASSES];
    let (mut mw, mut vw) = (vec![0f32; w.len()], vec![0f32; w.len()]);
    let (mut mb, mut vb) = (vec![0f32; CLASSES], vec![0f32; CLASSES]);
    let (beta1, beta2, eps) = (0.9f32, 0.999f32, 1e-8f32);

    for epoch in 1..=EPOCHS {
        let mut gw = vec![0f32; w.len()];
        let mut gb = [0f32; CLASSES];
        let mut total_weight = 0f32;
        for (x, (_, label)) in xs.iter().zip(samples) {
            let p = softmax(&logits(&w, &b, &present, x));
            let cw = class_weight[label.index()];
            total_weight += cw;
            for k in 0..CLASSES {
                if !present[k] {
                    continue;
                }
                let err = cw * (p[k] - if k == label.index() { 1.0 } else { 0.0 });
                gb[k] += err;
                gw[k * d..(k + 1) * d]
                    .iter_mut()
                    .zip(x)
                    .for_each(|(g, v)| *g += err * v);
            }
        }
        let scale = 1.0 / total_weight.max(1e-6);
        let t = epoch as i32;
        let (c1, c2) = (1.0 - beta1.powi(t), 1.0 - beta2.powi(t));
        for i in 0..w.len() {
            let g = gw[i] * scale + L2 * w[i];
            mw[i] = beta1 * mw[i] + (1.0 - beta1) * g;
            vw[i] = beta2 * vw[i] + (1.0 - beta2) * g * g;
            w[i] -= LEARNING_RATE * (mw[i] / c1) / ((vw[i] / c2).sqrt() + eps);
        }
        for k in 0..CLASSES {
            let g = gb[k] * scale;
            mb[k] = beta1 * mb[k] + (1.0 - beta1) * g;
            vb[k] = beta2 * vb[k] + (1.0 - beta2) * g * g;
            b[k] -= LEARNING_RATE * (mb[k] / c1) / ((vb[k] / c2).sqrt() + eps);
        }
    }

    PersonalModel {
        version: PersonalModel::VERSION,
        mean,
        inv_std,
        weights: w,
        bias: b,
        present,
        samples: counts.to_vec(),
        accuracy: None,
    }
}

fn logits(w: &[f32], b: &[f32], present: &[bool], x: &[f32]) -> Vec<f32> {
    let d = x.len();
    let mut z = vec![f32::NEG_INFINITY; b.len()];
    for k in 0..b.len() {
        if present[k] {
            z[k] = b[k]
                + w[k * d..(k + 1) * d]
                    .iter()
                    .zip(x)
                    .map(|(a, v)| a * v)
                    .sum::<f32>();
        }
    }
    z
}

fn softmax(z: &[f32]) -> Vec<f32> {
    let max = z.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let mut e = vec![0f32; z.len()];
    let mut sum = 0.0;
    for (o, &v) in e.iter_mut().zip(z) {
        *o = if v.is_finite() { (v - max).exp() } else { 0.0 };
        sum += *o;
    }
    e.iter_mut().for_each(|v| *v /= sum.max(1e-12));
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Синтетические «эмбеддинги»: у каждого класса свой центр, плюс шум.
    fn blob(label: Label, n: usize, seed: u64) -> Vec<(Vec<f32>, Label)> {
        let mut state = seed;
        let mut rnd = || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 40) as f32 / (1u64 << 24) as f32) - 0.5
        };
        (0..n)
            .map(|_| {
                let v = (0..EMBEDDING_SIZE)
                    .map(|i| {
                        let center = if i % 5 == label.index() { 1.0 } else { 0.0 };
                        center + rnd() * 0.8
                    })
                    .collect();
                (v, label)
            })
            .collect()
    }

    #[test]
    fn learns_separable_classes() {
        let mut data = blob(Label::Bark, 40, 1);
        data.extend(blob(Label::Whine, 25, 2));
        data.extend(blob(Label::NotDog, 30, 3));
        let model = train(&data).unwrap();
        assert_eq!(model.samples, vec![40, 25, 0, 0, 30, 0]);
        assert!(model.accuracy.unwrap() > 0.9, "{:?}", model.accuracy);
        let fresh = blob(Label::Whine, 5, 99);
        for (x, _) in &fresh {
            assert_eq!(model.decide(x).0, SoundClass::Whine);
        }
        // Классы, которых не было в обучении, модель не предсказывает.
        assert_eq!(model.probs(&fresh[0].0)[Label::Howl.index()], 0.0);
    }

    #[test]
    fn rejects_too_little_or_single_class() {
        assert_eq!(
            train(&blob(Label::Bark, 5, 1)).unwrap_err(),
            TrainError::TooFewSamples {
                have: 5,
                need: MIN_SAMPLES
            }
        );
        assert_eq!(
            train(&blob(Label::Bark, 30, 1)).unwrap_err(),
            TrainError::SingleClass
        );
    }

    #[test]
    fn survives_json_roundtrip() {
        let mut data = blob(Label::Bark, 15, 1);
        data.extend(blob(Label::NotDog, 15, 2));
        let model = train(&data).unwrap();
        let back: PersonalModel =
            serde_json::from_str(&serde_json::to_string(&model).unwrap()).unwrap();
        assert_eq!(back.decide(&data[0].0), model.decide(&data[0].0));
    }

    #[test]
    fn labels_have_stable_keys() {
        for l in Label::ALL {
            assert_eq!(Label::from_key(l.key()), Some(l));
        }
    }

    #[test]
    fn foreign_dog_is_its_own_class_but_not_my_dog() {
        let mut data = blob(Label::Bark, 40, 1);
        data.extend(blob(Label::ForeignDog, 40, 4));
        data.extend(blob(Label::NotDog, 30, 3));
        let model = train(&data).unwrap();
        let foreign = blob(Label::ForeignDog, 3, 77);
        assert_eq!(model.decide_label(&foreign[0].0).0, Label::ForeignDog);
        assert_eq!(model.decide(&foreign[0].0).0, SoundClass::Other);
        assert_eq!(model.samples.len(), Label::ALL.len());
    }

    #[test]
    fn model_from_before_foreign_dog_still_works() {
        let mut data = blob(Label::Bark, 30, 1);
        data.extend(blob(Label::NotDog, 30, 2));
        let mut old = train(&data).unwrap();
        // Модель прежней версии: пять классов, без «чужой собаки».
        let five = Label::ForeignDog.index();
        old.weights.truncate(five * EMBEDDING_SIZE);
        old.bias.truncate(five);
        old.present.truncate(five);
        old.samples.truncate(five);
        let old: PersonalModel =
            serde_json::from_str(&serde_json::to_string(&old).unwrap()).unwrap();
        assert_eq!(old.probs(&data[0].0).len(), five);
        assert_eq!(old.decide(&data[0].0).0, SoundClass::Bark);
    }
}
