# Guida a Vetro — per te

Questa pagina spiega, passo per passo, cosa stai costruendo e cosa devi fare tu. Se ti perdi, torna qui.

---

## 1. Vetro in una frase

Vetro trasforma il tuo tablet in un **secondo schermo** per il PC: lo colleghi (con il cavo o in Wi-Fi) e Windows lo vede come un monitor vero, a 120 Hz, con touch e penna.

Il codice lo scrivono gli agenti di Claude. **Tu fai tre cose:** dai il via a ogni tappa, fai le prove fisiche (cavo, penna, fotocamera) e dici "ok" alla fine di ogni tappa.

---

## 2. I file che contano

| File | Cosa c'è dentro | Quando leggerlo |
|---|---|---|
| `docs/it/GUIDA.md` | Questa guida | Ogni volta che non sai cosa fare |
| `ROADMAP.md` | L'elenco di tutte le tappe, dall'inizio alla fine | Per vedere il quadro completo |
| `docs/milestones/M0.md`, `M1.md`… | Cosa si fa in una tappa e a che punto siamo (tabella "Status" in fondo) | Quando vuoi sapere come sta andando |
| `ARCHITECTURE.md` | Come funziona tecnicamente | Solo se sei curioso (è in inglese e tecnico) |
| `docs/adr/` | Perché abbiamo preso ogni decisione | Solo se vuoi il "perché" di una scelta |

---

## 3. Le tappe

| Tappa | Cosa succede | Cosa fai tu | Tuo tempo |
|---|---|---|---|
| **M0** | Si prepara il PC, si crea il progetto su GitHub, si misura quanto è veloce un programma che esiste già (Apollo) | Installi i programmi (premi Run e "Sì"), controlli il tablet, fai la misura con la fotocamera | ~2 ore in tutto |
| **M1** | Nasce la prima versione vera di Vetro: tablet come monitor, in Wi-Fi e con il cavo, con touch e penna | Approvi il piano, provi l'app alla fine, misuri con la fotocamera | ~1 ora a fine tappa |
| **M2** | Colleghi il cavo e funziona da solo, senza toccare nulla sul tablet | Attacchi e stacchi il cavo quando te lo chiedono | ~30 min |
| **M3** | Se stacchi il cavo, Vetro passa al Wi-Fi senza interrompersi | Attacchi e stacchi il cavo | ~20 min |
| **v1.0 e oltre** | Rifiniture, store, italiano, Linux | Come sopra | — |

---

## 4. Una volta sola: prepara l'app Claude

1. **Sul PC** apri l'app **Claude**.
2. In alto scegli la scheda **Code**.
3. Scegli la cartella **`Progetto_senza_nome`** (è la cartella del progetto).
4. **Sul telefono** installa l'app **Claude** e accedi con lo stesso account: così ti arrivano le notifiche.

---

## 5. Come avviare una tappa (vale per tutte)

Esempio con la tappa M0. Per le altre cambia solo il nome (M1, M2…).

1. Nell'app Claude, scheda **Code**, cartella `Progetto_senza_nome`, apri una **nuova sessione**.
2. Nel selettore del modello scegli **Opus 5.5** e come effort **High**.
3. Come modalità dei permessi scegli **Auto**. (Così gli agenti lavorano senza chiederti il permesso per ogni comando normale. Le cose pericolose restano comunque bloccate: le trovi elencate in `.claude/settings.json`.)
4. Scrivi:

   ```
   /tappa M0
   ```

   e premi **Invio**.
5. Claude legge i documenti e ti scrive un **piano breve** (al massimo 15 righe, in italiano): cosa farà, in che ordine, cosa devi fare tu.
6. Leggilo. Se ti va bene, scrivi:

   ```
   ok
   ```

   Se qualcosa non ti torna, scrivilo con parole tue: Claude correggerà il piano.
7. Da qui in poi **puoi andare a studiare**. Claude lavora e ti scrive solo quando serve te (vedi punto 8).

---

## 6. Come riprendere una tappa

Capita in tre casi:
- la quota di Claude Pro è finita e poi si è ricaricata;
- hai chiuso l'app o spento il PC;
- ti arriva la notifica del **pilota automatico** (vedi punto 7).

Cosa fare:
1. Apri l'app Claude → **Code** → cartella `Progetto_senza_nome` → **nuova sessione**.
2. Modello **Opus 5.5**, effort **High**, permessi **Auto**.
3. Scrivi, per esempio:

   ```
   /tappa M1 riprendi
   ```

4. Claude rilegge lo stato da GitHub e riparte esattamente da dove era rimasto. Non si perde nulla, perché lo stato è salvato su GitHub e non nella chat.

---

## 7. Il pilota automatico

**Cosa fa:** tre volte al giorno (alle 9, alle 14 e alle 20) controlla tre cose: il PC è collegato a internet, è attaccato alla corrente, c'è lavoro pronto. Se tutte e tre sono vere, ti manda una notifica tipo:

> Vetro: posso riprendere la tappa M1? Prossimo lavoro: … Apri l'app Claude e scrivi /tappa M1 riprendi — oppure ignora.

**Cosa NON fa:** non comincia mai a lavorare da solo. Lavora solo se tu apri l'app e scrivi il comando del punto 6.

**Se il PC era spento:** quando riapri l'app, il pilota automatico ti chiede una volta sola (non ti riempie di notifiche).

**Se vuoi ignorarlo:** ignora la notifica. Non succede nulla.

**Se vuoi spegnerlo del tutto:** in una sessione Claude scrivi "disattiva le attività pianificate di Vetro".

Il pilota automatico viene creato all'inizio della tappa M1, e solo se dici di sì.

---

## 8. Le notifiche: cosa significano

| Notifica | Cosa vuol dire | Cosa fai |
|---|---|---|
| "posso riprendere la tappa…?" | C'è lavoro pronto | Se puoi, segui il punto 6. Altrimenti ignora |
| "serve una tua decisione" | Claude è bloccato su una scelta | Apri la sessione, leggi la domanda, rispondi (c'è sempre una risposta consigliata) |
| "servono le prove fisiche" | Bisogna attaccare un cavo, usare la penna o fare una foto | Apri la sessione: trovi l'elenco dei passi, numerati |
| "la tappa è pronta" | Il lavoro è finito e aspetta il tuo controllo | Segui il punto 10 |
| "il controllo delle prestazioni è fallito" | Una modifica ha reso Vetro più lento | Niente di urgente: Claude ci lavora alla prossima ripresa |

---

## 9. Il banco di prova notturno

Di notte (alle 2) Vetro può provarsi da solo sul tablet vero e misurare la sua velocità. Lo fa **solo se**:
1. il **tablet** è collegato al PC con il cavo e in carica;
2. il **PC** è attaccato alla corrente e acceso;
3. l'app **Claude** è aperta (anche ridotta a icona);
4. non stai usando il PC da almeno 10 minuti.

Lasciare il tablet collegato vuol dire "sì, puoi provare". Se non lo colleghi, la prova salta. Non cambia mai il codice: misura e basta.

---

## 10. A fine tappa: il tuo controllo

Quando arriva la notifica "la tappa è pronta":

1. **Prove fisiche.** Apri `docs/manual-tests/<tappa>.md`. I passi segnati **[fisico]** sono tuoi: falli uno alla volta e scrivi accanto "ok" oppure cosa non va.
2. **Prove sullo schermo con Antigravity.** I passi segnati **[schermo]** li fa Antigravity (punto 12).
3. **Revisione con ChatGPT.** Segui il punto 11.
4. **Il tuo ok.** Nella sessione della tappa scrivi `ok, tappa approvata`. Oppure scrivi cosa non ti piace: Claude lo sistema prima di chiudere.

---

## 11. Revisione con ChatGPT (passo per passo)

1. Apri il file `docs/reviews/<tappa>-chatgpt.md` (lo prepara Claude a fine tappa).
2. Seleziona tutto (**Ctrl+A**) e copia (**Ctrl+C**).
3. Apri **ChatGPT** e incolla (**Ctrl+V**). Il file contiene già le istruzioni per ChatGPT: non devi scrivere altro. Invia.
4. Copia tutta la risposta di ChatGPT.
5. Torna nella sessione Claude della tappa e scrivi:

   ```
   Ecco la revisione di ChatGPT:
   ```

   poi incolla la risposta e invia. Claude la legge e decide cosa sistemare.

---

## 12. Antigravity (passo per passo)

1. Apri **Antigravity** e apri la cartella `Progetto_senza_nome`.
2. Crea un nuovo compito per l'agente e incolla questo testo (cambia `m1` con la tappa giusta):

   ```
   Esegui uno alla volta i passi marcati [schermo] nel file docs/manual-tests/m1.md.
   Per ogni passo scrivi "OK" oppure cosa è andato storto, con uno screenshot se possibile,
   nel file docs/manual-tests/results/m1-antigravity.md. Non modificare nessun altro file.
   ```

3. Quando Antigravity ha finito, scrivi nella sessione Claude della tappa: `Antigravity ha finito i test`.

---

## 13. Quando Claude ti chiede di installare qualcosa

Per sicurezza, gli agenti non possono installare programmi da soli. Ti mostrano il comando, e lo esegui tu:
1. Il comando appare in un riquadro con il pulsante **Run**. Premilo. (Oppure copialo in **PowerShell**.)
2. Se Windows chiede **"Consentire a questa app di apportare modifiche al dispositivo?"**, premi **Sì**.
3. Quando ha finito, scrivi nella sessione: `fatto`.

**La finestra del Firewall di Windows (dalla tappa M1, al più tardi quando nella M1-6 avvii tu `vetro-engine`):** la prima volta che il motore parte, Windows chiede se permettergli di usare la rete. **Prima**, controlla che il Wi-Fi di casa sia impostato come **Privata**: Windows di solito segna come *Pubblica* una rete appena collegata, e in quel caso il firewall blocca il tablet. Vai su Impostazioni → Rete e Internet → Wi-Fi → (la tua rete) → **Tipo di profilo di rete** e scegli **Privata**. **Poi**, nella finestra del firewall, spunta solo **Reti private** e togli la spunta da **Reti pubbliche**, poi premi **Consenti accesso**. Così il tablet raggiunge il PC a casa, ma non sulle reti di bar, treni o università. Con il cavo USB (più avanti, nella M1-14) servirà lo stesso controllo: te lo spiegheremo allora. Se hai sbagliato, scrivilo a Claude: ti dà il comando per sistemarlo.

**Scaricare FFmpeg (tappa M0-1):** Claude ti dà il comando `tools\fetch-ffmpeg.ps1` da incollare in PowerShell, dalla cartella del progetto. Scarica circa 80 MB da GitHub e controlla che il file sia quello atteso: se non lo è, si ferma senza installare nulla. Alla fine ti dà un secondo comando, per impostare `FFMPEG_DIR`: incollalo, poi apri un nuovo terminale.

---

## 14. Preparare il tablet (una volta sola, tappa M0)

Servono solo sul **tuo** tablet, per lo sviluppo. Chi userà Vetro non dovrà farlo.

1. **Attiva le Opzioni sviluppatore:** Impostazioni → **Informazioni sul tablet** → **Informazioni software** → tocca **7 volte** "Numero build". Se chiede il PIN, inseriscilo.
2. **Attiva il debug:** Impostazioni → **Opzioni sviluppatore** → accendi **Debug USB** e **Debug wireless**.
3. **Collega il tablet al PC** con il cavo. Sul tablet compare "Consentire il debug USB?": spunta **Consenti sempre da questo computer** e premi **OK**.
4. **Controlla il tethering:** Impostazioni → **Connessioni** → **Hotspot mobile e tethering**. C'è la voce **Tethering USB**? Scrivi a Claude "sì, c'è" oppure "no, non c'è".
5. Scrivi a Claude la **versione di Android**: Impostazioni → Informazioni sul tablet → Informazioni software → "Versione Android".

---

## 14 bis. Provare l'app sul tablet (tappa M1-6)

Questo è il controllo fisico della tappa M1-6: collegare il tablet al motore. Serve il tablet con il debug USB attivo (sezione 14), il cavo USB per installare l'app e il Wi-Fi di casa, **lo stesso** per PC e tablet. Il PC deve avere il Wi-Fi impostato come **Privata** (sezione 13).

**Cosa aspettarti, in onestà:** il motore, per ora, manda "frame finti" (byte senza un vero video: il codificatore arriva con la M1-7). Quindi **non vedrai ancora le barre colorate in movimento**. Quello che puoi controllare adesso è: l'abbinamento funziona, in alto a sinistra compare **Streaming** e il tablet resta a **120 Hz**. Con i dati finti il decodificatore può rifiutare i frame e dopo qualche secondo comparire "Connection failed": è atteso, non è un guasto. Scrivi a Claude cosa hai visto.

1. **Scarica l'app.** In PowerShell (Claude ti dice il numero della prova automatica, qui `NUMERO`). Si scarica in una cartella temporanea, fuori dal progetto:
   `gh run download NUMERO --name vetro-debug-apk --dir $env:TEMP\vetro-apk`
2. **Installa sul tablet** (cavo USB collegato, ti chiede di consentire il debug: Consenti):
   `adb install -r $env:TEMP\vetro-apk\app-debug.apk`
   Se compare `INSTALL_FAILED_UPDATE_INCOMPATIBLE`, sul tablet c'è già una versione firmata in un altro modo: scrivi `adb uninstall dev.vetro.app` e rifai l'installazione. Attenzione: questo cancella anche gli abbinamenti salvati nell'app, quindi dovrai abbinare di nuovo.
3. **Avvia il motore.** La prima volta Windows mostra la finestra del firewall: spunta solo **Reti private** (sezione 13).
   `cargo run -p vetro-engine --release -- --no-vdd --pair`
   Il motore sceglie da solo l'indirizzo del PC sul Wi-Fi. Se invece dice che non ne trova uno, scrivi `ipconfig`, cerca sotto "Scheda Wi-Fi" la riga **Indirizzo IPv4** (per esempio `192.168.1.20`) e aggiungi `--bind 192.168.1.20:47800` al comando.
4. **Il motore stampa una riga** che inizia con `vetro://pair?...`: è il codice di abbinamento. Vale **5 minuti** e **una volta sola**. Non incollarlo in chat, nelle issue, in un sito o in un messaggio a nessuno. Lascia il motore acceso.
5. **Passa il codice al tablet.** Il motore stampa un testo, non ancora un disegno QR (il QR vero arriva con la M1-12). Due modi:
   - **Con il cavo (più comodo):** apri una **seconda finestra PowerShell** (il motore resta acceso nella prima), copia la riga del motore e scrivi il comando qui sotto, incollando la riga dove c'è `RIGA`. Tieni le virgolette come sono (doppie fuori, singole dentro).
     `adb shell am start -a android.intent.action.VIEW -d "'RIGA'"`
     Sul tablet si apre Vetro con il codice già nel campo.
   - **A mano:** apri Vetro sul tablet, premi **Pair a new PC** e scrivi la riga nel campo.
   Se hai un QR di quella riga, basta inquadrarlo con la camera (la prima volta Android chiede il permesso **Fotocamera**). Il QR va fatto solo con un programma che funziona senza internet, **mai** con un sito: il codice è un segreto.
6. **Sul tablet premi Pair.** Sullo schermo intero compare "Connecting...", poi **Streaming**, e sotto `panel 120 Hz, vsync 120/s`. Nel terminale del motore compare `session with ... (newly paired)`.
7. **Scrivi a Claude cosa hai visto**: le due righe del tablet (testo e Hz) e se il PC ha scritto `newly paired`. Se compare **Not paired**, il codice era scaduto o già usato: riavvia il motore (punto 3) e riprova dal punto 4. Per uscire dallo schermo intero usa il gesto "indietro". Tre dita insieme sullo schermo mostrano o nascondono la scritta.

Due limiti per ora: finché il motore non ricorda gli abbinamenti (M1-12), a ogni riavvio del motore devi rifare l'abbinamento; e la ricerca automatica del PC sul Wi-Fi (mDNS) non è ancora nell'app, quindi si usa il codice.

---

## 15. Come fermare tutto

- **Fermare il lavoro adesso:** nella sessione premi il pulsante **Stop**.
- **Fermare tutto finché non riapri:** chiudi l'app Claude. Anche il pilota automatico e le prove notturne si fermano.
- **Spegnere il pilota automatico per sempre:** scrivi in una sessione "disattiva le attività pianificate di Vetro".

---

## 16. Se qualcosa va storto

| Problema | Cosa fare |
|---|---|
| Claude dice che la quota è finita | Normale con il piano Pro. Aspetta la notifica del pilota automatico, poi segui il punto 6 |
| Non ricordo a che punto siamo | Apri `docs/milestones/<tappa>.md`, guarda la tabella "Status" in fondo |
| Il PC è lento mentre studio | Scrivi nella sessione: "rallenta, sto usando il PC". Gli agenti useranno meno risorse |
| Una notifica non è chiara | Apri la sessione e chiedi: "spiegami in parole semplici cosa ti serve" |
| Ho chiuso la sessione per sbaglio | Nessun problema: punto 6 |

---

## 17. Parole difficili

| Parola | Significato semplice |
|---|---|
| **Tappa (milestone)** | Un pezzo di progetto con un traguardo chiaro |
| **Agente** | Una copia di Claude che fa un lavoro preciso |
| **Sessione** | Una conversazione con Claude nell'app |
| **PR (pull request)** | Una proposta di modifica al codice, che va controllata prima di entrare |
| **CI** | I controlli automatici che GitHub fa su ogni modifica |
| **Latenza** | Il ritardo tra quando qualcosa cambia sul PC e quando lo vedi sul tablet |
| **HIL** | La prova automatica con il tablet vero collegato |
| **Quota** | Quanto puoi usare Claude prima di dover aspettare |
| **UAC** | La finestra di Windows che chiede "Consentire modifiche?" |
