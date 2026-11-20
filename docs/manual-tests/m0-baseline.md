# M0 — Misura di riferimento con Apollo + Artemis

> Owner checklist (Italian). Steps marked **[fisico]** are for the owner, **[schermo]** can be run by Antigravity. Results go to `bench/m0-baseline.md`.

Tempo previsto: ~45 minuti. Prima di iniziare servono: tappa M0-3 fatta (tablet preparato) e il file `tools/latency-timer.html` (lo crea Claude nella tappa M0-4).

## A. Installare (una volta)
1. **[schermo]** Sul PC scarica l'installer più recente di **Apollo** dalla pagina delle release ufficiali: `github.com/ClassicOldSong/Apollo/releases`. Installalo (Windows chiederà "Consentire modifiche?": **Sì**).
2. **[fisico]** Al primo avvio Apollo apre una pagina nel browser (`https://localhost:47990`) e chiede di creare un nome utente e una password. **Scegli tu la password e scrivila solo tu**: né Claude né Antigravity devono conoscerla.
3. **[fisico]** Sul tablet scarica l'APK più recente di **Artemis** da `github.com/ClassicOldSong/moonlight-android/releases` e installalo (il tablet chiederà di consentire l'installazione da questa fonte: consenti).

## B. Collegare
4. **[fisico]** Tablet e PC sulla stessa rete Wi-Fi a 5 GHz. Apri Artemis: il PC compare nella lista. Toccalo: appare un PIN.
5. **[fisico]** Nella pagina di Apollo sul PC, sezione **PIN**, inserisci il PIN.
6. **[fisico]** In Artemis → Impostazioni:
   - risoluzione: quella nativa del tablet (2960×1848) o l'opzione che crea il display virtuale con la risoluzione del tablet;
   - FPS: **120**;
   - codec: **HEVC**;
   - bitrate: **80 Mbps**;
   - attiva le **statistiche a schermo** (performance overlay).

## C. Misurare in Wi-Fi
7. **[fisico]** Avvia lo streaming del **Desktop**. Su Windows compare un secondo monitor. Verifica in Impostazioni → Schermo che sia in modalità **Estendi**.
8. **[schermo]** Apri `tools/latency-timer.html` in **Edge** due volte, in due finestre, e trascina ogni finestra **sul suo schermo** (una sul portatile, una sul monitor del tablet). Subito prima delle foto ricarica le due finestre una dopo l'altra (tasto F5), poi in ognuna **clicca o premi F** per lo schermo intero (Esc per uscire).
9. **[fisico]** Metti il telefono in modo che inquadri **insieme** lo schermo del portatile e il tablet. In modalità **Pro** imposta il tempo di scatto a **1/500 s** (o almeno 1/250 s); se non c'è la modalità Pro, usa la modalità **Sport**. Scatta **almeno 40** foto (anche a raffica), a qualche secondo di distanza una dall'altra. Claude scarterà da solo le foto in cui il quadrato accanto a «F» è grigio su uno dei due schermi.
10. **[fisico]** Copia le foto nella cartella `bench/m0-photos/wifi/` del progetto. Claude leggerà i numeri da solo: in ogni foto, numero del portatile meno numero del tablet = ritardo.
11. **[fisico]** Fai uno screenshot sul tablet con le statistiche di Artemis visibili e mettilo nella stessa cartella.

## D. Misurare con il cavo (solo se il tablet ha "Tethering USB")
12. **[fisico]** Collega il cavo, attiva **Tethering USB** sul tablet.
13. **[fisico]** Scrivi a Claude "tethering attivo": ti dirà quale indirizzo aggiungere a mano in Artemis.
14. **[fisico]** Ripeti i passi 7–11 salvando le foto in `bench/m0-photos/usb/`.

## E. Alla fine
15. **[fisico]** Scrivi nella sessione della tappa: `misure M0 fatte`.
16. **[fisico]** Quando Claude ha salvato i risultati, **disinstalla Apollo** (Impostazioni → App), così il suo driver non si scontra con quello di Vetro.
