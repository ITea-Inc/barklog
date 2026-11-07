// Уведомление на другие телефоны аккаунта, когда собака лает слишком долго.
//
// Записывающий телефон пишет снимок в users/{uid}/live/state; в документе есть
// activeBoutStartMs / activeBoutMs — идущий период лая. Если он длиннее порога,
// шлём один пуш на этот период на все устройства аккаунта, кроме записывающего.
// Требует тарифа Blaze (Cloud Functions). Развёртывание: firebase deploy --only functions

const { onDocumentWritten } = require("firebase-functions/v2/firestore");
const { initializeApp } = require("firebase-admin/app");
const { getFirestore } = require("firebase-admin/firestore");
const { getMessaging } = require("firebase-admin/messaging");

initializeApp();

/** Через сколько непрерывного лая присылать уведомление. */
const BARK_ALERT_MS = 10 * 60 * 1000;

exports.liveAlerts = onDocumentWritten(
  { document: "users/{uid}/live/state", region: "europe-west1" },
  async (event) => {
    const after = event.data?.after;
    const live = after?.data();
    if (!live || live.status !== "recording") return;
    if (live.activeBoutStartMs == null || (live.activeBoutMs ?? 0) < BARK_ALERT_MS) return;

    // Один пуш на период: ключ = начало записи + начало периода.
    const key = `${live.startedAtMs}:${live.activeBoutStartMs}`;
    const db = getFirestore();
    const fresh = await db.runTransaction(async (tx) => {
      const doc = await tx.get(after.ref);
      if (doc.get("alertedBoutKey") === key) return false;
      tx.update(after.ref, { alertedBoutKey: key });
      return true;
    });
    if (!fresh) return;

    const { uid } = event.params;
    const devices = await db.collection(`users/${uid}/devices`).get();
    const tokens = devices.docs
      .filter((d) => d.id !== live.deviceId && d.get("fcmToken") && d.get("alerts") !== false)
      .map((d) => d.get("fcmToken"));
    if (tokens.length === 0) return;

    const minutes = Math.floor(live.activeBoutMs / 60000);
    await getMessaging().sendEachForMulticast({
      tokens,
      notification: {
        title: `Собака лает уже ${minutes} мин`,
        body: "Откройте BarkLog, чтобы посмотреть, что происходит дома",
      },
      android: { priority: "high", notification: { channelId: "alerts" } },
    });
  },
);
