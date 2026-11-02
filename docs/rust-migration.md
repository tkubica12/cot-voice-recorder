# Migrace Windows a cloudove casti do Rustu

## Rozhodnuti a rozsah

Postupna migrace do Rustu, nejdrive Windows a potom cloud. Nejde o jednorazovy
prepis: nejprve overit prinos na funkcnim pilotu, potom prenaset chovani a data.
Android, Azure sluzby a Bicep zustavaji zachovany. Posledni rozhodnuti uzivatele:
zustat u klasicke ACA; na ACA Express se v teto migraci neprechazi.

## Aktualni progress

| Cast | Stav |
| --- | --- |
| Plan, vetev a prvni commity | Ulozeno a pushnuto |
| Audio jadro a C#/Python parity | Implementovano a overeno |
| Zdroj externich crates | crates.io vyslovne schvalene jako projektova vyjimka |
| Nativni Windows zaklad | Tray, izolovana instance a DPAPI implementovany; lokalni self-test a C# DPAPI parity prosly |
| Mereni nativniho zakladu | Dokonceno: 30 startu a 301 vzorku za pet minut; vysledky nize |
| CI nativniho inkrementu | Windows i Linux prosly na commitu `4b040a8` |
| OAuth/token-store/PubSub klient | Native sign-in/out, realtime/history/clipboard a explicit installed-mode wiring implementovany; skutecny Google/clipboard E2E zbyva |
| Diktovani/recovery UI | Native microphone/hotkeys/overlay/paste controller, oba enginy, journal, polishing a outbox implementovany; real-world a rozsahlejsi fault/E2E akceptace zbyva |
| History cache | JSON/retence/200 entries, atomicka persistence, native list/text a manual copy latest/selected/original implementovany; C# file parity overena |
| Settings a instalator | Vsechna C# JSON pole, nativni Settings/History/Recovery, DPI fonty, per-user autostart a kompatibilni IPC; installer sestaven, nebyl instalovan |
| Rust cloud API, worker, cleanup | API/AI/PubSub/live relay, processors/finalization/queue loop, cleanup, bootstrap/CLI a source-only image wiring implementovany; image CI proslo, uplna akceptace zbyva |
| Hosting | Klasicka ACA zustava: API, KEDA queue worker a hourly cleanup Job; Express byl uzivatelem zrusen |

Nativni zaklad neni kompletni klient. Jeho rychlost a pamet nelze porovnat
s plnohodnotnym C# klientem jako dukaz hotove migrace.

Windows je silnejsi kandidat. Jednorazovy snimek beziciho procesu z 1. listopadu
2026 ukazal priblizne 163 MiB working setu a 201 MiB private bytes. Neni to
reprezentativni idle baseline ani mereni startu. Private bytes nejsou totez co
fyzicka rezidentni pamet.

Soucasne WPF nelze jednoduse odlehcit zapnutim NativeAOT. Microsoft dokumentuje
[nekompatibilitu WPF s trimmingem](https://learn.microsoft.com/en-us/dotnet/core/deploying/trimming/incompatibilities#wpf).
V cloudu ocekavame mensi image, rychlejsi start procesu a nizsi pametovou rezii,
nikoli automaticky rychlejsi AI prepis reci. AI, sit a Azure infrastruktura
zustavaji stejne. API aktualne drzi jednu teplou repliku; worker skaluje na nulu.
Pridelene prostredky API jsou 0.5 vCPU / 1 GiB a workeru 0.25 vCPU / 0.5 GiB;
jde o konfiguraci, nikoli namerenou spotrebu nebo slib nizsi ceny.

Bezpecnost neni automaticky zisk. C# a Python uz maji spravovanou pamet. Rust
pridava silnejsi kontrolu vlastnictvi a nekterych soubeznych pristupu, ale
nenahradi spravnou autentizaci, recovery ani bezpecne zachazeni s Win32.

## Aktivace a Git workflow

Uzivatel 1. listopadu 2026 schvalil ulozeni tohoto planu, zahajeni prace a
postupne commity a pushe do samostatne vetve `refactor-rust`.

- Pracovat v teto vetvi v existujicim checkoutu, ne v nove worktree.
- Commitovat ucelene zmeny po odpovidajicim overeni a pushovat pouze tuto vetev.
- Nemergovat do `main`, nevytvaret release ani nenasazovat do produkce.
- Nemazat nebo neprepisovat osobni historii, tokeny, recovery ani cloudova data.
- Pouzivat lokalni synteticka data a oddeleny testovaci stav.
- Zachovat soucasnou C#/Python implementaci jako referenci a rollback, dokud
  neni konkretni nahrada kompletni a overena.
- Externi Rust zavislosti pridavat teprve po overeni schvaleneho Cargo registry
  a dostupnosti verzi. Prvni zaklad muze pouzivat pouze standardni knihovnu
  a sestavovat se offline bez pristupu k registry.

Schvaleni prace neni schvaleni placenych AI volani, instalace novych cloudovych
sluzeb, zmen opravneni, produkcniho nasazeni nebo oslabeni akceptacnich cilu.

Uzivatel nasledne vyslovne potvrdil: "crates.io je pro tento projekt schvalene
jako vyjimka". Externi Cargo zavislosti teto migrace proto mohou pouzivat
crates.io; projektove `.cargo\config.toml` pouziva sparse protocol. Tato vyjimka
nemeni predepsane Microsoft feeds pro Python, npm nebo NuGet a nepovoluje
GitHub source installs, vypnuti bezpecnostnich kontrol nebo produkcni nasazeni.

## Poradi migrace

| Etapa | Konkretni vysledek | Podminka pokracovani |
| --- | --- | --- |
| 1. Vychozi mereni | Opakovatelna mereni soucasneho Windows klienta a backendu, katalog chovani a formatu | Dostatecna baseline pro ferove porovnani |
| 2. Windows pilot | Nativni tray, OAuth/DPAPI, PubSub -> clipboard a zakladni audio/streaming cesta | Prokazany prinos v presne vymezenem rozsahu pilotu |
| 3. Kompletni Windows klient | Settings, History, overlay, oba rezimy diktovani, recovery, audit outbox, instalator | Funkcni a datova kompatibilita; znovu zmereny kompletni klient |
| 4. Rust cloud API | HTTP API a WebSocket relay na soucasne klasicke ACA; puvodni Python worker docasne zustava | Stejny kontrakt, privatni egress/UAMI, streaming kompatibilita a ulozena data |
| 5. Worker a cleanup | Leases, replay, legacy pipeline, finalizace, notifikace a uklid | Poruchove scenare a bezpecny prechod bez dvojiteho zpracovani |

Pro Windows je navrzene nativni UI bez WebView, protoze hlavnim motivem je
nizka rezie. Platformni Win32 operace maji zustat izolovane od bezpecne domenove
logiky. Pro backend je navrzene Tokio/Axum. Nejde o schvaleni konkretni verze
zavislosti nebo o tvrzeni, ze jsou jiz dostupne ve schvalenem registry.

Azure ma moderni Rust knihovny pro
[Identity, Blob a Queue](https://learn.microsoft.com/azure/developer/rust/azure-sdk-library-package-index).
Adaptery pro Tables a Web PubSub potrebuji samostatne overeni. PubSub umoznuje
[REST/WebSocket integraci bez SDK](https://learn.microsoft.com/azure/azure-web-pubsub/overview#why-use-azure-web-pubsub).
Pouzivat stale managed identity, privatni Storage a serverem zvolene AI endpointy.
Nerozsirovat pristup Windows klienta primo na Azure kvuli pohodli prepisu.

## Hosting: klasicka ACA zustava

Uzivatel zvazoval ACA Express kvuli rychlejsimu cold startu, ale po overeni
omezeni vyslovne rozhodl: "zustaneme u klasicke ACA, nebudeme prepinat".
Toto je platne cilove rozhodnuti; Express neni dalsi povinna migracni etapa.

Zachovat soucasnou topologii: HTTP/WebSocket API v klasicke ACA, queue worker
s `azure-queue` KEDA a scheduled hourly cleanup Job. Zachovat VNet/private
endpoints, privatni Storage/Speech a user-assigned managed identity.
Nemenit kvuli prepisu min replicas, scale rules, domeny nebo produkcni opravneni.
Rust image a resource tuning budou overeny samostatne bez soubezne zmeny hostingu.

Rust/Python benchmark pouzije stejny hosting a workload. Proces -> readiness
neni HTTP scale-from-zero ani stop -> text. Nizsi pamet sama nesnizuje ucet bez
zmeny alokace; runtime prepis nezrychluje samotne MAI/LLM. Akceptacni prahy
a poruchove scenare zustavaji beze zmen.

## Neprekrocitelne podminky kompletni migrace

- Zachovat HTTP tvary, stavove kody a WebSocket protokol z OpenAPI. Soucasne
  pytesty proti FastAPI nebo xUnit proti C# samy o sobe neoveruji Rust.
  Prenest scenare do testu Rustu a externich kontraktnich testu.
- Zachovat Google ID token jako backend bearer, allowlist, PKCE/state,
  obnovu tokenu a soucasnou DPAPI CurrentUser ochranu.
- Overit cteni starych recovery v1-v4, nastaveni, historie a pending auditu
  na syntetickych fixturech. Overit take data vytvorena Rustem proti staremu
  klientovi, pokud ma byt mozny rollback bez migrace dat.
- Provisionalni text pouze zobrazovat. Nikdy nevlozit neuplny text po selhani
  nebo prekroceni stop budgetu. Recovery nikdy automaticky nevklada.
- Zachovat neaktivujici overlay, klavesove zkratky, vyber cile pri startu
  hold-to-talk a pri stopu hands-free, ochranu focusu a clipboard sekvence,
  jedine Ctrl+V bez Enter a viditelny fallback.
- Zachovat checkpointy, absolutni PCM cursory, presne poradi vzorku a reconnect
  replay. ACK paketu neni potvrzeny prepis a nesmi uvolnit durable audio.
  Nepretvaret soucasny checkpointovy model na slib bezeztratoveho archivu.
- Zachovat omezeni front, chyby disku/kvoty, 48hodinovou retenci a 256 MiB
  recovery kvotu. Nesnizovat pamet zahazovanim audia.
- Zachovat durable audit outbox, fixni expiraci od puvodniho dokonceni,
  idempotentni retry a 409 pri konfliktu. Audit nesmi emitovat automaticky
  clipboard event ani blokovat paste.
- Zachovat Blob/Queue/Table formaty, ETag a vlastnictvi leases, jejich obnovu,
  ordered full replay, legacy pipeline a semantiku finalizace/cleanup.
- Pro prechod workeru nepoustet dva alternativni enginy na stejne recording
  bez prokazaneho fencing/lease protokolu. Zmena jazyka neresi distribuovane zavody.
- Zachovat per-user instalaci, jedinou instanci a jediny autostart mechanismus,
  upgrade bez force-kill a absenci OAuth konfigurace/secrets ve verejnem baliku.
- Znovu overit klavesnicovou pristupnost, DPI a ctecku obrazovky v nativnim UI.
  Funkcni pilot bez techto povrchu neni kompletni nahrada.

## Goal Card: mereny Rust pilot

### OBJECTIVE

Card: RUST-PILOT | Version: 1 | Type: experiment | Owner: uzivatel

Status: lokalni prace a Git workflow schvaleny; kompletni pilot zustava
`Draft - not ready to run`, dokud nejsou overeny vsechny jeho prerequisites.
Dependency-free zaklad a vychozi lokalni mereni mohou pokracovat samostatne.

Rozhodnout zvlast pro Windows a backend, zda prinos Rustu ospravedlnuje dokonceni
migrace. Pilot neni produkcni nahrada.

H1: Windows dosahne nize uvedenych cilu pri zachovani pilotnich funkci.
H2: backend dosahne pametovych a startovacich cilu bez zhorseni spravnosti
a odezvy. Namerene nesplneni cile hypotezu vyvraci. Nefunkcni prostredi
znamena blokaci, nikoli vyvraceni.

### OUTPUT

Dve srovnani, Windows a backend, s nezkracenymi vysledky mereni, rozsahem pilotu
a zaverem "pokracovat / nepokracovat". Podklady `rust-pilot-windows.json`
a `rust-pilot-backend.json` v artefaktech relace; zaver v konverzaci.
Tento soubor udrzovat jako plan a strucny stav migrace. Neuplne podklady oznacit
jako neuplne; nenahrazovat chybejici mereni odhadem.

### DONE WHEN

Cisla jsou navrzene pilotni cile, ne namerene vysledky ani zaruky.

| Metrika | Windows | Backend na Linuxu |
| --- | --- | --- |
| Pamet v idle | Private bytes <=80 MiB a <=50 % baseline | RSS <=50 % baseline |
| Start procesu, p95 | <=1 s a <=70 % baseline | Proces -> readiness <=1 s a <=50 % baseline |
| Odezva | Lokalni pripravenost mikrofonu nezhorsena o vice nez 10 % | p95 vybranych API operaci nejvyse 110 % baseline |
| Stabilita | Po hodinove nahravce a navratu do stejneho idle stavu narust private bytes <=10 MiB | Zadne nove chyby pri stejne zatezi |

1. **C01 - Ferova baseline.** Stejny stroj, release build, vstupy, historie
   a konfigurace; 30 startu a tri serie zateze. Overit ulozene vysledky.
   Pri rozdilnych podminkach mereni zopakovat a zneplatnit C02.
2. **C02 - Verdikt H1/H2.** Vyhodnotit vsechny predepsane serie, ne nejlepsi
   beh. Dukazem jsou podklady a vypocet metrik. Chybejici mereni doplnit;
   prekroceny cil priznat, nesnizovat automaticky prah. Po zmene znovu C02.
3. **C03 - Spravnost pilotu.** Porovnat stejne scenare proti soucasne
   implementaci, vcetne odmitnuti neplatne autentizace, reconnectu
   a opakovanych pozadavku. Dukazem jsou scenare a jejich vystupy.
   Opravit odchylky a zopakovat C03 i dotcena mereni C01/C02.
4. **C04 - Rozhodnuti a uklid.** Zaver odpovida dukazum, zadne pomocne
   procesy nezustaly spustene a produkce nebyla zmenena. Overit skutecny
   stav pred ukoncenim; pri selhani dokoncit povoleny uklid a znovu C04.

U kazdeho checku zaznamenat `NOT_RUN`, `PASS`, `FAIL` nebo `BLOCKED` a dukaz.
Zadny check neprosel pouze tim, ze existuje plan nebo Rust projekt.

### QUALITY

Mensi neuplny pilot nesmi byt vydavan za dukaz spotreby kompletni nahrady.
Lokalni benchmark s fake AI neni mereni skutecne cloudove latence ani ceny.
Bezpecnost a spravnost maji prednost pred usporou prostredku.

Mereni startu procesu nesmi byt zameneno s casem prvni autentizovane cloudove
transkripce. CPU merit jako rozdil procesoroveho casu za skutecny interval,
oddeleny od celkoveho casu CPU od startu. Rozlisit procento jednoho logickeho
jadra a procento celeho stroje. U vzorku 30 startu je p95 pilotni odhad,
nikoli produkcni SLA.

### CONTEXT

Autoritativni podklady: `openapi\voice-recorder.yaml`,
`docs\windows-dictation.md`, `docs\mobile-streaming.md`,
`docs\architecture.md` a soucasne testy.

Preflight musi overit Rust/MSVC, Linuxove merici prostredi, schvaleny Cargo
registry pro externi zavislosti a minimalni lokalni pruchod.
Soucasne testy jsou katalog ocekavaneho chovani; jejich pruchod proti C#/FastAPI
neoveruje Rust. Zdroj externich Rust zavislosti je nyni schvaleny crates.io
prostrednictvim vyslovne projektove vyjimky uvedene v aktivaci.

Referencni prikazy z komponent:

```powershell
# Z windows:
dotnet test VoicePrompt.sln -c Release

# Z backend:
uv run pytest --ignore=tests/integration
```

Pri pouziti existujicich zavislosti zachovat lockfiles a schvalene feeds.
Nespoustet CI krok, ktery znovu resolvuje zavislosti proti verejnemu registry,
jako workaround omezeni spravovaneho stroje.

### CONSTRAINTS

Jednat v `refactor-rust` v rozsahu Aktivace a Git workflow. Vlastni testovaci
data a procesy oznacit tak, aby bylo mozne uklidit pouze je. Existujici
aplikaci ani osobni stav pri mereni nerestartovat nebo menit.

Cloveka pozadat pouze o login/consent, schvaleny zdroj nebo instalaci chybejiciho
toolingu, placene operace, produkcni zmeny nebo zmenu cilu a limitu.
Nedostupne prerequisites nezakryvat teoretickym vysledkem.

Fixni jsou cil, hypotezy, checky, prahy a vstupy. Materialni zmena potrebuje
novou schvalenou verzi. Git push s neznamym vysledkem overit read-backem
remote ref pred opakovanim; nikdy nepouzivat force-push.

### STAGES

Preflight -> baseline -> nejmensi funkcni Windows pilot -> backend pilot ->
porovnani a uklid. Kompletni migrace je samostatny navazujici build cil,
s etapami a neprekrocitelnymi podminkami uvedenymi vyse.

Priklad opravne vetve: pokud pamet roste kvuli neomezene audio fronte, opravit
omezeni a backpressure, potom zopakovat pametovy test i kontrolu zachovani
vzorku. Nezahazovat audio kvuli lepsim cislum.

### STOP-CAPS

Navrzeny limit pilotu: 16 hodin aktivni prace, nejvyse 72 hodin celkem a tri
opravne cykly, vcetne mereni a uklidu. Bez novych placenych sluzeb.
Tyto pilotni limity nejsou terminem dokonceni cele migrace.

`DONE` pouze s dolozenymi verdikty a splnenymi checky; jinak `BLOCKED`,
`CAPPED` nebo `CANCELLED`. Pri ukonceni zachovat neuplne podklady, presne
chybejici dukazy a bezpecny navazujici krok. Limit neni alternativni uspech.

Prvni mereni ma potvrdit, zda uspora pretrva se zapnutym recovery a streamingem.

## Stav

1. listopadu 2026: vetev `refactor-rust` vytvorena z `25390dc`.
Plan ulozen. Rust 1.99.1, Cargo, rustfmt a clippy jsou jiz nainstalovane,
ale jejich adresar nebyl v PATH relace. Docker CLI je dostupne; Docker engine
nebezi. Linuxove mereni a schvaleny Cargo registry pro externi zavislosti
nejsou zatim overeny. Pilotni checky C01-C04 jsou `NOT_RUN`.

Prvni inkrement: dependency-free audio zaklad Rustu a read-only procesove
mereni. Neni to jeste nativni tray aplikace ani nahrada backendu.

Prvni audio zaklad je implementovany v `rust\`: WAV encoder, legacy/strict
validator a PCM chunker vcetne snapshot/restore. Offline build, rustfmt,
clippy a 15 Rust audio testu prosly. Externi parity runner porovnal primo
aktualni C# zdroj a Python validator s Rustem na syntetickych vstupech,
vcetne fragmentace, obnovy, overlap flags, byte cursoru a malformovanych WAV.
Odpovidajicich 15 C# a 7 Python referencnich testu take proslo.
To neoveruje DPAPI manifesty, HTTP/WebSocket protokol nebo celeho klienta.

Prvni procesove mereni trvalo 300 sekund a obsahuje 301 vzorku existujiciho
Windows klienta 1.5.1. Prumerne private bytes byly 202.58 MiB, p95 202.59 MiB;
prumerny working set 168.87 MiB a CPU 0.1042 % jednoho logickeho jadra.
Scenar byl pouze pozorovana existujici relace, nikoli kontrolovany idle se
syntetickou historii. Raw vystup je `windows-process-initial.json` v artefaktech
relace; neobsahuje texty, audio, tokeny nebo nastaveni aplikace a neni commitovany.
C01 proto nadale neni `PASS`: chybi kontrolovane scenare, opakovane serie,
startovaci mereni a Linuxova baseline. C02-C04 kompletniho pilotu jsou `NOT_RUN`.

Dalsi etapa byla spustena po explicitnim schvaleni crates.io. `windows` 0.62.2
byl overen v registry, Cargo resolver vybral Rust 1.85 kompatibilni graf a
aktualizoval lockfile. Nativni kod je oddelen od audio jadra zakazujiciho unsafe.
Lokalni tray self-test overil skutecne vytvoreni ikony/skryteho okna, nezmeneny
focus pri startu, izolovane instance, DPAPI a graceful uklid.
C# protector a Rust obousmerne desifruji synteticke envelopy se stejnou
`VoicePrompt.DPAPI.v1` entropy; poskozena data a neplatne startup options
jsou odmitnute. Skutecne tokeny, clipboard, audio ani produkce se nepouzily.

Mereni nativniho zakladu pouziva oddelene procesy a explicitni readiness
marker. Neslouzi jako dukaz H1: chybi Google/PubSub, mikrofon/streaming,
recovery, audit outbox a kompletni UI. Po techto portech je potreba zopakovat
ferove porovnani proti stejnym C# scenarum.

## Vysledky omezeneho nativniho inkrementu

Kod nativniho inkrementu je commit `4b040a8`, pushnuty do `refactor-rust`.
[Rust CI](https://github.com/tkubica12/cot-voice-recorder/actions/runs/41312664282)
uspesne dokoncilo Windows i Linux job; Windows zahrnuje C# DPAPI parity.

Collector provedl 30 novych procesu a potom 301 vzorku po dobu 299.99 sekund.
Kazdy proces mel vlastni instance key, overeny PID a atomicky publikovanou
readiness po inicializaci tray a syntetickem DPAPI health checku.

| Metrika | Nativni zaklad bez auth/ASR |
| --- | --- |
| Prumerny start procesu -> pozorovana tray/DPAPI readiness | 118.22 ms |
| p95 startu, nearest-rank, 30 vzorku | 157.68 ms |
| Prumerny working set | 10.20 MiB |
| Prumerne private bytes | 1.59 MiB |
| p95 private bytes | 1.70 MiB |
| Prumerne CPU jednoho logickeho jadra | 0.0104 % |

Raw podklady jsou `native-foundation-startup.json` a
`native-foundation-memory.json` v adresari `native-foundation-measurements`
artefaktu teto relace. Obsahuji scope, source revision, binary hash a vsechny
vzorky. Nejsou commitovane. Smoke-test collector ma vlastni oddelene podklady.

To jsou vysledky **neuplneho nativniho zakladu**, ne slib spotreby cele aplikace
a ne ferove srovnani se soucasnym plnohodnotnym C# klientem. Collector tuto
neporovnatelnost explicitne zaznamenava. Start neobsahuje Google, sit, audio
nebo cloud a nejde o OS cold-cache benchmark. Plne H1/H2 a C01-C04 zustavaji
nedokoncene; jejich prahy se nemeni.

Vsechny vlastni merene procesy skoncily graceful, readiness/staging soubory
byly uklizeny a puvodni VoicePrompt zustal spusteny. Pilot nebyl ponechan
na pozadi jako bezici recorder nebo autonomni migration loop.
Nasleduje port OAuth/token-store JSON a realtime klienta do izolovaneho stavu,
potom zachycovani/streaming, durable recovery a audit. Cloudove etapy zustavaji
navazujici praci; produkcni nasazeni neni schvalene.

## Dalsi Windows inkrement: klientsky protokol a token store

`client-protocol` nyni obsahuje PKCE z OS CSPRNG, konstantni porovnani state,
minimalni Google authorization URL, C# PascalCase cached-token JSON, read-only
JWT claims, klasifikaci refresh a single-owner auth state machine. Ta publikuje
nove tokeny az po uspesnem durable zapisu a nevraci OAuth access token jako
backend ID token.

Loopback listener drzi OS-prirazeny port na 127.0.0.1 po celou dobu flow,
overuje callback pred success odpovedi a ma timeout/cancellation/header bounds.
PubSub parsing podporuje raw, JSON a textove envelopy a omezene FIFO dedupe.

Nativni encrypted store obousmerne cte/pise synteticke soubory ze skutecneho
C# TokenStore, vcetne rotace/prepisu. JSON i DPAPI binding jsou zachovany.
Zamerne je chyba korupce/desifrovani viditelna a soubor zachovany, nikoli
maskovana jako uspesna prazdna cache.

HTTP token transport a asynchronni auth orchestrace nyni existuji: omezene
form POST requesty, system TLS, deadline/body bounds, bez nasledovani redirectu,
serialized refresh/sign-in/sign-out a off-thread cache/callback prace.
Lokalni flow overuje callback -> exchange -> persistence -> pouze ID token;
refresh revocation a cancellation jsou explicitni. Nepouzily se Google/Azure
ani skutecne tokeny.

WebSocket transport nyni skutecne vyjednava `json.webpubsub.azure.v1`, prijima
completed eventy, omezuje zpravy/buffery a podporuje backpressure, URL renewal,
ping/pong, suspend/resume/reconnect/stop a zruseni spojeni po zmene auth.
Lokalni WebSocket fixture overila subprotocol, doruceni a ukonceni pres stop
i sign-out; jde o synteticky endpoint, nikoli Azure production.

V teto predchozi protokolove etape jeste chybel system-browser sign-in propojeny
s native UI a clipboard/history integrace realtime klienta. Tato etapa
neprihlasuje uzivatele a nepouziva jeho konfiguraci, tokeny ani cloud. Jeji
unit/parity testy nejsou end-to-end test plneho Windows klienta.

Authenticated API klient nyni zvlada negotiate a transcript fetch s Google
ID tokenem, structured RFC 9457 problem details, terminal 403, bounded 5xx retry
a presne jeden refresh po 401. Tyto vetve jsou overene proti lokalnim HTTP
fixturem; malformed success payload neni akceptovan jako hotovy transcript.
Neni to produkcni ani kompletni client/cloud end-to-end overeni.

History cache nyni zachovava C# JSON pole, original/final text, 48hodinovou
retenci, newest-first limit 200 a replace-by-ID. Nativni atomicky zapis a
obousmerna kompatibilita souboru jsou overeny syntetickymi fixturemi proti
skutecnemu C# HistoryStore. Pause/dictation-busy/audit-source policy neprovadi
autocopy.

## Native delivery inkrement

Explicitni `--state-directory` nyni zapojuje native tray do auth/realtime,
atomicke historie a clipboardu. `--oauth-config` je take explicitni; vychozi
foundation nezjistuje osobni stav ani konfiguraci. Connected mode nesmi pouzit
foundation readiness marker, aby se neoznacovalo nove mereni jako stare.
Nativni status okno obsahuje sign-in/out, realtime status, History list/text,
manual copy latest/selected/original, pause persistence a reconnect.
Neni to jeste kompletni Settings, diktovani, Recovery nebo instalator.

Coordinator serializuje fetch/save/copy. Dedupe potvrzuje az uspesny durable
zapis; pri chybe site nebo disku nezablokuje retry. Clipboard busy ma pet
omezenych pokusu, jine chyby jsou terminalni a viditelne. Historie zustava
manualnim fallbackem. Policy se kontroluje i pred skutecnym UI zapisovanim
a cancellation zrusi odlozeny clipboard request. Audit nikdy neprovadi autocopy.
Notifikace a diagnostika neobsahuji text prepisu.

Settings zachovavaji vsechna aktualni C# pole/defaults, Rust take nezname pole
kvuli rollbacku. Corrupt/invalid file neni uspesna prazdna konfigurace.
Periodic cleanup trvale uplatnuje retenci historie. Testy pouzivaji jen
synteticke temporary cesty a nikde neinstaluji/upgradeuji skutecnou aplikaci.

Lokalne overeno: 60 Rust testu celeho workspace, release a Clippy bez warnings,
vcetne skutecneho local HTTP authenticated fetch -> coordinator -> cache ->
fake clipboard. Desktop-only lifecycle ma pet skupin: missing state,
synteticky native History list a hidden focus, zachovani corrupt
settings/history/tokens, skutecna aktivace explicitniho stavu v normalnim modu
a odmitnuti mislabeled readiness. Skutecny systemovy
clipboard i OAuth jsou v self-testu zakazane. Neni tim prokazano produkcni
Google/clipboard/paste ani feature-matched performance.

Native delivery commit `a406337` je pushnuty. Linux CI proslo; Windows CI
odhalilo zavislost noveho policy-race testu na presnem poradi kratkych timeru.
Oprava testu pouziva explicitni udalosti na fetch/busy hranicich, nikoli
predpoklad o rychlosti stroje. Nejde o obchazeni kontroly ani snizeni cile.

Navazujici realtime fault inkrement ma sest novych primych Rust testu.
Renewal deadline je absolutni uz pred handshake; handshake ji neprodluzuje.
Ping/pong write ma tri-sekundovy limit a reaguje na stop/auth/renewal.
Overena plna event fronta pri stop/sign-out, pomaly handshake az po expiraci,
malformed i oversized frame, ukonceny consumer a skutecne zablokovany duplex
transport. Cely workspace ma lokalne 66 passing Rust testu; pred oznacenim
CI za uspesne je nutne overit novy Windows i Linux beh.

CI pro `0efe6f6` overilo Rust test/build na obou OS, ale Windows DPAPI runner
narazil na modalni startup chybu pri invalid-instance testu. Tato fixture
nyni vyslovne pouziva `--self-test`; realny GUI startup error zustava viditelny.
Nejde o selhani DPAPI parity nebo odmitnuti neplatneho instance key.

## Recovery codec inkrement

Rust nyni cte binarni manifest v1-v4 vcetne C# UTC ticks a aktualniho v4
confirmed cursoru/textu, context audia a frozen audit completion. V1/v2 neznamy
cursor zustava nula, v1 nema vymysleny context. Nova serializace je v3/v4.
SHA-256 a canonical PCM WAV checks pouzivaji existujici audio jadro.
Provisional/ACK nejsou confirmation: audio-release seznam vznikne jen po
platne confirmed cursor transition. Audit konflikt nemuze zmenit puvodni
completion time; timestampy se neztraci presunem do sekund.

Nativni encrypted fixture hook ma explicitni absolute manifest.enc layout,
DPAPI CurrentUser, path/reparse guards a atomicky zapis. Neni to concurrency/
quota-aware journal a v native tray se Recovery zatim necte. Zbyva port root
kvoty 256 MiB vcetne staging/orphans, active journals, publication-before-delete,
streaming/fallback enginu, outbox uploadu a UI.

Lokalne: 72 Rust testu, release/Clippy, C# reference bez warnings a ctyri nove
recovery parity skupiny prosly. Aktualni C# v3/v4 journal -> Rust je byte-exact,
Rustem znovu zapsany DPAPI manifest cte skutecny C# RecoveryStore se stejnym
snapshotem. Synteticke v1/v2 take ctou oba a po upgrade zustava stejny snapshot.
Corrupt input nezmeni puvodni file. Zadny test nepouzil osobni recovery/audio,
Google nebo systemovy clipboard. Recovery codec neni plny Recovery E2E.

Recovery codec a navazujici opravy jsou pushnuty jako `9b25162`.
[Rust CI](https://github.com/tkubica12/cot-voice-recorder/actions/runs/41330838670)
proslo na Windows i Linuxu, vcetne Windows OAuth/settings/history/DPAPI a nove
recovery v1-v4 parity. Testove race i modalni fixture problem z predchozich dvou
behu jsou opravene a potvrzene timto naslednym CI; neuspesne behy zustavaji
v historii a nejsou vydavany za success.

Nejblizsi aktivni prace: quota-aware native recovery journal s root ownership,
active journal guards, atomickymi checkpointy a publication-before-delete;
potom live/legacy dictation engines, capture/hotkey/overlay/paste a audit outbox.
Rust cloud API/worker/cleanup, kompletni UI/installer a feature-matched C01-C04
jsou stale nedokoncene. Zadny neprovedeny real-world check neni `PASS`.

## Native recovery journal

`NativeRecoveryStore` nyni implementuje create/open/checkpoint, verified read,
legacy result, streaming confirmation, frozen audit, list, scoped delete a cleanup.
Root ma OS-exclusive zero-byte `writer.lock`; stejny process sdili gate, journal
ma jedineho aktivniho vlastnika. Snapshot neni zdroj nasledneho prepisu: kazda
mutace pod gate znovu cte aktualni manifest. Pri soubehu capture a result update
se neztrati nove chunks ani jiz potvrzeny text.

Quota zahrnuje stary target, orphan/unpublished files i prave zapisovane staging.
Test meri skutecne DPAPI ciphertext sizes a vyvola chybu o jediny byte pred
manifest publication: novy audio orphan zustane, stary manifest je stejny a
nasledny retry presne obnovi vzorky. Pri selhani confirmation publication se
pending audio nesmaze. Po uspesne publication se mazou jen fully confirmed
audio files; stejne legacy result retry je idempotentni i bez dalsiho zapisu.

Cleanup zachovava source semantiku `>48 h` od updated-at, aktivni a unreadable
journals vynechava. Delete ma flat-layout preflight a odmitne nested directory
pred jakymkoliv smazanim. Realne Windows junction fixtures potvrdily odmitnuti
read/write/delete bez zmeny cilovych dat. Vlastnictvi rootu je overeno dvema
skutecnymi native procesy; po graceful uvolneni muze navazat novy owner.

Osm novych native testu proslo, stejne jako rozsirenych sedm recovery
compatibility/fault skupin. Rustem vytvorene legacy/streaming/audit journals cte
skutecny C# store a plny native store cte C# v1-v4 fixtures. C# ale novy
writer.lock nerespektuje: testy nezpracovavaji recording soubezne dvema enginy.
Skutecny cutover vyzaduje budoucni application single-instance/installer parity.
Native tray zatim nezachytava mikrofon a journal do dictation UI neni zapojen.

Journal commit `f653195` ma successful Windows i Linux CI:
[run](https://github.com/tkubica12/cot-voice-recorder/actions/runs/41334590864).

## Dictation client protocols

Nyni existuje authenticated WAV transcribe, literal-edit refine a immutable audit
PUT client. WAV retry je nejvyse jeden transient pokus navic, refinement zadny;
401 ma jediny refresh. Read-body truncation se take omezenou cestou retryuje,
malformed success se nikdy nepovazuje za text. Audit zachovava original completion
a overuje receipt ID/source/48h expiry; Python microsecond precision je porovnana
bez ztraty lokalnich C# ticks.

Proxy WebSocket overuje `ready` protocol/cursor, ma bounded PCM/JSON, 25s deadlines
a auth-change cancellation pro send i receive. Raw PCM, commit a finish, delta/
intermediate versus confirmed/done jsou overeny realnym local WebSocket fixturem,
vcetne refresh-once pri handshake 401, invalid ready a sign-out. ACK neni confirmed.
Streaming session engine a capture/paste tim jeste nejsou implementovany.

Legacy audio snapshot JSON zachovava C# PascalCase/base64 a processed cursor.
Text stitching zachovava exact prefix, empty adjacency a short-word seam dedupe.
Aktualni Unicode tables nebyly automaticky kompatibilni: tabulky normalizace/
kategorii jsou pinute na .NET 8 profile. Skutecny C# matcher overuje vsechny validni
BMP scalars a representative supplementary compatibility znaky; dalsi fixtures
overuji stitching, append a fragmented PCM snapshots. Nejde o odhad parity.

Opravena je take auth publication race. Cache/state publication probiha pod
session lockem; po delayed blocking refresh se live token znovu kontroluje.
Kontrolovana concurrency fixture nejprve dokonci sign-out a az potom obnovi
refresh future: stary bearer ani SignedIn se nesmi vratit. Osobni tokens a placene
AI nebyly pouzity. Native engines/UI/outbox a Rust cloud zustavaji otevrene.

Protocol commit `68b35c0` ma successful Windows i Linux
[CI](https://github.com/tkubica12/cot-voice-recorder/actions/runs/41338054588),
vcetne nove kompletni BMP/stitch/legacy snapshot parity. Lokalni workspace ma
93 passing testu, release/fmt/Clippy a dosavadni client/lifecycle parity prosly.
Navazuje native legacy/streaming session engine a auth/backend-bound outbox.

## Native dictation session engines

`LegacySession` nyni pouziva skutecny encrypted journal, single-producer chunker,
JSON snapshot a nejvyse dva ASR requesty. Vysledky jsou viditelne pouze v contiguous
capture-order prefixu. Terminal auth podrzi backlog, transient errors maji bounded
backoff. Stop timeout vraci `None`, ne partial text; shutdown zrusi requesty a
encrypted pending audio zustava pro restart. Fragmentace capture a ordering jsou
overeny fake ASR proti realnemu store.

`StreamingSession` oddeluje durable capture od live PCM transportu, checkpointuje
po dvou sekundach a provisional deltas pouziva jen v preview. Commit confirmation
publikuje completed cursor/text pred audio deletion. Known sent-commit hranice
odmita impossible future cursor. Reconnect zacina persisted confirmed byte, ne
network ACK. Real store/fake model fixtures overily presne vzorky pri full replay,
partial confirmed prefix po failure a batch Recovery s puvodnim context/seam.
Recovery nikdy automaticky nepaste. Conn rollover, quiet commits, finish/drain
a cancellation zustavaji oddelene od capture a Stop budgetu.

Binding neni novy format: puvodni `backend_base_url + newline + email` je zachovan.
Bound API overuje email atomicky se ziskanym ID tokenem a sleduje auth-change pri
request/body read; jiny uzivatel/backend nemuze dostat puvodni audit/audio.
Test overil wrong-email reject pred network I/O a sign-out za pending requestu.

Jde o knihovni engine povrchy, ne kompletni Windows aplikaci. Native microphone,
hotkey/overlay/focus/clipboard/paste controller, polishing/outbox scheduling,
full Settings/Recovery a installer se jeste musi napojit. Zadny test nespustil
mikrofon, neprecetl osobni clipboard/tokeny ani nezavolal placene AI.

## Native device/UI wiring a build-only installer

Native controller nyni zapojuje mikrofon (WinMM, stejna WaveIn cesta a format jako
C#), bounded queue 50x40 ms, oba enginy, nonactivating overlay, hold/toggle/cancel,
History a Recovery. Target selection je pri startu hold nebo stopu toggle;
focus/process/thread/clipboard sequence a modifier guards chrani jediny Ctrl+V
bez Enter. Sequence je zachycena uvnitr clipboard publication locku. Overload,
disk/device chyby a stop timeout zachovavaji Recovery a nepaste partial text.

Settings ma aktualni C# pole a standardni native controls, keyboard navigation,
DPI-specific system fonts a per-monitor bounds. Save while idle zachovava nezname
JSON fields, atomicky persistuje, znovu pripoji native client a pouziva jen HKCU Run.
Native History umi cloud Refresh bez autocopy, Clear s confirmation a obe verze textu.
Legacy dictation History ID je puvodni `dictation-<N-uuid>`, streaming dashed UUID.
Explicit discard pred delivery odstrani svuj History entry i audio/outbox; completed
audit Recovery zachovava original fixed completion a puvodni final/raw text.

Opt-in polishing pouziva single physical request, original-indexed literal unique
edits, bounded rolling windows a frozen delivery. Late odpovedi nemeni pasted text;
unsent tail neni AI failure. Outbox drenuje oddelene po 30 s jen v idle pod puvodni
URL/newline/email binding. Ack smaze outbox, transient/409 ho podrzi, original
completion fixuje 48h expiry. Audit neblokuje paste a nevysila autocopy notification.

Prime porovnani se skutecnym backend wire modelem odhalilo chybejici
`character_count` ve summary receiptu; Rust ho uz nevymysli jako required field.
Local `--backend-model` parity cte aktualni Pydantic model a skutecny Rust parser.
Rust-only CI tento volitelny backend-environment check neprovadi; C#/Rust text a
checkpoint parity dale bezi samostatne.

Per-user native Inno installer je sestaven, nikoli spusten nebo instalovan.
Bundle obsahuje jen exe a repo icon, zadnou konfiguraci/secrets nebo user data.
Source zachovava AppId, jeden Run value a stejny C# mutex/auto-reset events;
graceful --quit timeout abortuje upgrade bez force-kill. Obousmerny real-process
C#/Rust IPC test pouziva nahodne fixture namespace. `--installed` runtime cesta
je napsana, ale nebyla aktivovana nad osobnim stavem; isolated mode ho odmita.

Lokalni evidence: 115 Rust tests, fmt/Clippy/release, sest native lifecycle
groups vcetne hidden Settings value/focus parity, pet platform/DPAPI/IPC groups,
ctyri text/audio parity a actual backend summary check prosly. Installer build
prosel se stavajicim Inno compilerem. Windows real Google/microphone/paste,
screen-reader/high-DPI/installer upgrade, feature-matched performance a cloudove
Rust etapy zustavaji neprovedene. Tyto checky nejsou `PASS`.

Native wiring commit `3a63cef` ma successful Windows i Linux
[CI](https://github.com/tkubica12/cot-voice-recorder/actions/runs/41351359019).
Per-user installer se pouze sestavil; jeho spusteni/upgrade nad osobnim stavem
ani zadne AI/production nasazeni se neprovedlo.

## Rust cloud foundation

`voiceprompt-cloud` je nyni clen workspace. Implementuje bounded/cached Google
JWKS a skutecnou RS256 verification s issuer/audience/exp/iat/nbf, verified email
a single-email allowlist. Vychozi production konfigurace vyzaduje endpointy a auth;
fake/Azurite/auth-disabled nastaveni do production nepusti.

ACA UAMI credential pouziva injected private endpoint/header a explicitni
user-assigned client ID, ne account key, client secret nebo desktop Google token.
Blob/Table/Queue REST primitives zachovavaji bearer, version/date a ulozene klice.
Table replace/delete odmita wildcard/missing ETag; Queue renew vraci novy receipt,
stale receipt je explicitni failure a gone delete je idempotentni se zaznamem.
Storage response/identity/token bodies nejsou v diagnostice.

Dev RSA keys se generuji jen v pameti testu; zadny private fixture key se
necommitoval. Dev crates jsou z approved crates.io a resolver overil stavajici
lockfile/MSRV-compatible graf. Nejnovejsi XML crate potreboval Rust 1.86, proto je
zvolena overena 0.38.3. MSRV compiler samotny stale nebyl samostatne spusten.

Devet cloud testu proslo proti realnemu local HTTP/JWKS/RSA: rejected unsigned/
HS256/wrong claims, cached keys, UAMI selection, conditional Table 412 a Queue
receipt renewal/gone semantics. Nejde o skutecne Azure private-endpoint/UAMI
testy ani hotove HTTP/worker API. Cloud je zatim library foundation, bez
production bootstrapu, kompletniho entity/storage codec, routeru, MAI relay,
leased replay/finalization/cleanup a container/deployment wiring.

Cloud foundation commit `4ce466a` ma successful Windows i Linux
[CI](https://github.com/tkubica12/cot-voice-recorder/actions/runs/41353049879).

## Cloud entity a wire codecs

Nove Rust domain/wire models zachovavaji recording/chunk/transcript pole, nullable
timestampy, stare chunked/legacy-overlap/refinement defaults a forward state machine.
Table codecs zachovavaji partition/row keys, DateTime microsecond profile,
samples, lease/finalization flags a audit digests. Digest a mobile audio checks
overuji exact contiguous 10s layout i legacy stride456000/max480000 a odmitaji
overlap-only tail. Conditional memory repository sdili stejny CAS konflikt jako
skutecny Table primitive; nejde o production fallback.

Actual Python Table writers -> Rust a Rust -> actual Azure SDK deserializer ->
Python readers prosly ve trech syntetickych parity skupinach. Nejnovejsi test
zahrnuje 2.7 miliardy samples: Azure SDK vraci pro Edm.Int64 EntityProperty a
puvodni Python int(value) nad nim selhal. Rust proto pise ordinary Int32 pro
bezne counters a Int64 jen pro large values; Python rollback reader ma tesne
navazanou _integer/_optional_integer opravu. Starsi neopravena Python image ma
stale tento Int64 problem a neni vydavana za overeny rollback.

Tri nove prime SDK reader testy a souvisejici recording/transcript API regressions
prosly (22 pytest scenaru), targeted Mypy nema chyby, Rust cloud ma 14 passing testu.
Backend CI zachovava existing Microsoft-feed uv.lock, prestava re-resolvovat na
public PyPI a zapojuje bidirectional SDK parity s realnym Rust oracle.
Full Rust HTTP/relay/worker/cleanup, container wiring a performance/real-world
checks jsou stale zbyle aktivni casti celeho cile.

## Cloud services, HTTP a AI adapters

Rust services a skutecny Axum router nyni obsluhuji create/get/chunk/complete,
transcript get/list, idempotentni audit, neperzistentni dictation transcribe/refine
a realtime negotiate. Zpracovani zachovava durable publication, replay completion,
CAS rezervaci finalize pred queue send a watchdog; samotny worker jeste neexistuje.
Readiness zustava false pred bootstrapem. Nejde o nasaditelnou nahradu.

Batch Azure Speech a alternativni Foundry transcription pouzivaji UAMI a presne
multipart/AI endpointy. Diktovani ma oddeleny clean/strict response profil a 20s
limit. Refinement zachovava doslova existujici prompt, JSON data a ochranu anchors,
numbers, identifiers, scripts, whitespace a context echo. Pool ma 4/2 sloty;
timeout nebo zruseny HTTP waiter neuvolni slot probihajici fyzicke prace.
Provider bodies se nepropaguji do chyb/logu a zadna placena AI nebyla zavolana.

Web PubSub REST zachovava konfigurovane user/group/hub a token TTL, MI scope
a omezenou joinLeaveGroup roli. Completion posila pouze preview, ne full/raw text;
windows_dictation audit nesmi emitovat clipboard completion.

Lokalni workspace testy prosly: 150 (35 cloud). Prime lokalni HTTP fixture overuji
real Rust routing, multipart audio/hints, Foundry prompt/payload, provider
terminal/transient chyby, PubSub token/user paths a zrusene/timed-out work slots.
Externi actual-Python parity prosla pro SDK entity (3 skupiny), edit protection
a presny prompt (2), audit envelope/digest/preview/retrieval a UTF-8 byte quota (2).
Audit zustava max 1 MiB UTF-8 na kazdy text, ne 1 MiB Unicode znaku. Python navic
poklada U+001C..U+001F za whitespace; Rust helper tuto kompatibilitu zachovava.
Locked release, fmt a Clippy prosly. CI pro novy inkrement se overuje samostatne.

Live MAI relay, continuous/legacy worker, cleanup, production bootstrap a
container/klasicka ACA wiring jsou nadale aktivni nezavrsene casti celeho cile.
Neprobehl skutecny Azure/OIDC/device/paste/installer test ani feature-matched
performance acceptance. Prahy a neprodukcni hranice se nemeni.

## Live MAI upstream a dictation relay

Native Rust MAI provider zachovava server-owned MI bearer, resource-host rewrite,
PCM16k/session.update, auto versus explicit language, bounded 2 MiB frames,
15s initialization a 3s writes/close. Authenticated Axum WebSocket route zachovava
ready/delta/intermediate/confirmed/done/error frames i 4401/4403/4400/4429/4503
rejection codes; respektuje configured stream enable flag a ctyri session sloty.

Relay udrzuje bounded 32 commits, oddeluje ACK/hypothesis od confirmed text,
vydava out-of-order model completions v capture poradi a zachovava absolute
byte_end. Audio packet max16KiB, exact posledni sample do hodinoveho audio capu,
55min session a 30s final drain jsou zachovany. Cas session je stale zdrojovy
limit, ne namereny hodinovy acceptance workload.

Proslo 162 workspace testu (47 cloud), release/fmt/Clippy a actual asynchronous
Python relay -> Rust cursor/confirmation parity. Prime socket E2E overuje exact
PCM, auth/query/capacity errors a sorted confirmed/done. Prime casove testy
zmerily skutecny 15s chybejici session.created a 30s chybejici completion:
provisional text ani ACK neprinesly confirmed/done a slot se po timeoutu uvolnil.
Nejde o placeny ani Azure E2E. Predchozi commit 536e3a0 ma potvrzene Windows/Linux
Rust CI i backend CI; CI tohoto dalsiho inkrementu se overuje samostatne.

Nasleduje leased continuous replay a legacy/finalization/cleanup. Produkcni
bootstrap/container wiring, distributed-fault acceptance, real-world a
feature-matched performance zustavaji nedokoncene.

## Continuous worker lease a PCM primitives

Rust nyni zachovava claim/update-owned/renew/release protokol stavajiciho
continuous workeru ve stejnych recording lease polich. Claim resetuje replay
progress na nulu; owner/expiry se kontroluji pri kazde CAS aktualizaci.
Sixteen concurrent claimants maji jedineho viteze; po expiraci a prevzeti
puvodni owner nemuze publikovat ready ani zrusit lease sveho nastupce.

Pri navazani byla opravena Rust repository race: po uspesnem Table PUT/POST
nasledny GET mohl vratit data a ETag jineho writeru. Nyni vraci vlastni response
ETag a vlastni hodnoty; missing/wildcard ETag je explicitni chyba, ne fallback
na cizi snapshot. Zachovava se If-Match a 412 conflict, bez unconditional retry.

Strict PCM extractor respektuje ancillary RIFF chunks a nepredpoklada vzdy
44byte header. Durable chunk reader kontroluje checksum, exact sample metadata,
gap/redundant range a zahazuje jen proven duplicate source samples legacy
overlapu. Contiguous replay zachovava kazdy sample. Jde o primitives, ne jeste
o bezici kompletni worker; supervisor, queue receipt heartbeat, stream replay
loop, legacy finalization a cleanup zustavaji aktivni praci.

Proslo 169 workspace testu (53 cloud, vcetne 4 lease/replay a 2 own-ETag
HTTP scenaru), locked release/fmt/Clippy a ctyri actual C#/Python audio parity
skupiny. Predchozi live-relay commit 1ae68df ma potvrzene oba Rust CI OS a
backend CI. Azure ani placeny model nebyly pouzity. Na produkcnich recordings
se zadny alternate engine nespousti.

## Continuous replay processor a queue receipts

Rust continuous processor nyni spojuje durable upload windows do jednoho MAI
streamu, packetuje presne640B PCM a zachovava configured 1..8x replay pacing,
poll/idle/heartbeat/session limits. Failure zacina dalsi pokus od sample0,
ne od ACK/hypothesis ani predchoziho streamed_samples. Po poslednim durable
segmentu vysila jediny commit; source samples nevklada jako silence a model
mezi segmenty neresetuje.

Tri tasky obsluhuji audio, transcript a recording/queue heartbeat. Shutdown/fault
zastavi dalsi work a pripoji rozbehnute persistence/receipt mutations pred
release; fyzicky in-flight write se neschova za cancellation. Pouze final
completed text po EOF muze dostat unique owner raw path a owned-CAS asr_ready.
Late owner/queue receipt loss nesmi publikovat hotovy recording.

Queue ports nyni maji real Azure receive/renew/delete a odpovidajici memory
visibility/redelivery model. Receive vraci jediny message. QueueReceipt serializuje
renew/delete a persistuje vzdy novy pop receipt; stale delete neodstrani
message prevzaty nastupcem, outage zustava explicitni. Upstream sender je
serializovany a oddeleny od receiveru, aby heartbeat/audio/text behy neblokovaly
cteni modelu neomezene.

Proslo 178 workspace testu (62 cloud), locked release/fmt/Clippy. Ctyri socket
processor testy overuji failed-attempt full replay, exact contiguous samples,
legacy stride456000/max480000 bez duplicate/silence/reset, stop pri chybejicim
uploadu a lease takeover. Ctyri queue testy overuji visibility, redelivery,
receipt rotation, concurrency a outage. Predchozi 0bdb5ab ma potvrzene
Windows/Linux Rust CI i backend CI; dalsi inkrement ma vlastni CI check.

To neni kompletni QueueWorker/CLI ani finalization/cleanup. Legacy chunked engine,
durable finalization/notification, cleanup/orphans, bootstrap/container wiring,
full distributed faults a feature-matched/human acceptance nadale zbyvaji.
Nevykonano zadne produkcni nasazeni ani placene AI.

Windows CI commitu b2e91af odhalilo timeout pri startu powershell.exe pro
syntetickou junction fixture (15s), nikoli selhani Rust/C# recovery formatu.
Fixture preferuje dostupny pwsh, ma explicitni 60s setup budget a overuje,
ze vytvoreny objekt je skutecne junction. Sedm lokalnich recovery parity skupin
proslo; CI opravy se overuje samostatne. Zadny aplikacni timeout ani acceptance
prah nebyl zmenen nebo skryt.

## Legacy text a canonical Blob fencing foundation

Server overlap stitching pouziva skutecny Python NFKC/full-casefold/word profil,
ne Windows invariant-lowercase helper. Prime Python parity overuje vsechny
assigned Unicode scalars, seam korpus a presne uuid5 namespace publication IDs.
622 CJK Extension I letters z Unicode15.1 bylo nutne doplnit ke kompatibilnim
15.0 tabulkam; Windows Unicode profil se kvuli tomu nemeni.
PubSub event IDs jsou nyni stabilni z recording_id stejne jako v Python pipeline.

Whole-recording refiner zachovava vlastni puvodni system/user prompt a deployment,
nezamenuje se s bounded literal-edit dictation refinerem. Exact prompt a skutecny
lokalni HTTP payload byly overeny. Novy caseless0.2.2 a sha1.11 byly queried v
approved Cargo exception a resolved s locked normalization0.1.23, bez upgradu
zbytku grafu. Caseless nema declared MSRV; build overuje instalovany compiler,
ne samostatne rust1.85.

Native Blob acquire/renew/release a leased PUT/If-None-Match primitives i memory
lease model chrani canonical paths pred stale writerem. Fencing se kontroluje
na Blob serveru, ne jen pred PUT v recording Table. Tato cast je foundation
pro finalization; kompletni legacy engine, finalizer a supervisor wiring
tim jeste nejsou hotove. Provedeny jsou synteticke/local REST a takeover testy,
nikoli Azure nebo placeny model.

Proslo 182 workspace testu (66 cloud), locked release/fmt/Clippy a tri actual
Python pipeline text/prompt/UUID parity skupiny, vcetne all-assigned-Unicode
normalization corpus. Existujici SDK entity a audit byte parity zustavaji passing.
CI junction-fixture opravy dabeff2 je potvrzene successful na Windows i Linux.
Zdrojove canonical Blob fencing primitives nejsou vydavany za plnou distributed
finalizer akceptaci; je treba je jeste napojit na legacy/finalize processor.

## Legacy pipeline, finalization a sequential QueueWorker

Rust chunked processor pouziva exact checksum, glossary hints, source-compatible
batch AI a vlastni audio Blob lease. Text musi byt ulozen conditional CAS pred
leased delete audia. Streaming recording nesmi spustit druhy legacy engine.
Chyba inference ponechava accepted row a durable audio pro retry.

Canonical finalization zachovava raw id.txt, transcript UUID5.txt, puvodni model/
refinement flags, preview a prvni fixed completion/expiry. Finalizer drzi
renewable lock v raw/finalize/id.lock. Canonical raw/final bytes jsou publish-once
If-None-Match: i stale owner po ztrate lease nemuze prepsat uz vyhrane texty.
Metadata vychazeji z persisted canonical body, ne z posledniho speculative
model outputu. Crash po body PUT a pred metadata muze resume bez druheho model
callu; jde o explicitni source mechanismus pro bezpecnejsi finalizaci, ne tvrzeni
o kompletni distributed fault akceptaci.

Dokonceny recording s neACKovanou PubSub notifikaci drzi durable retry. Retry
ma stabilni event UUID5 a nepresouva completion/expiry. Ztratene doruceni se
nezahazuje po vycerpani inference attempts; legacy i continuous cesta ackuji
stejne pole stream_delivery_completed. Teprve notification ACK povoluje cleanup
continuous source audio/raw attempt. Audit tim neni zmenen a nikdy nenotifikuje.

QueueWorker dostava jeden message, rozlisuje continuous/legacy/finalize,
klasifikuje terminal/transient/pending, respektuje configurable attempts/backoff,
uklada rotated receipts a zije i po receive/renew/delete faults. SIGTERM watch
je stop-responsive; CLI signal/bootstrap wiring jeste zbyva.

Proslo 192 workspace testu (76 cloud), locked release/fmt/Clippy. Nove prime
HTTP/memory pipeline/worker testy overuji full legacy queue->ASR->finalize,
stitch/refine, missing-chunk grace, immutable-body crash resume, transient/exhausted
work a permanent notification retry, vcetne zachovani continuous audia do ACK.
Predchozi 6fcb639 ma potvrzene Windows/Linux Rust CI a backend CI.
Cleanup/orphans/active-work fencing, production bootstrap, source-only ACA image
wiring a uplna acceptance jsou stale aktivni casti celeho cile.

## Cleanup, cloud CLI a source-only container wiring

Cleanup ma bounded/plne paginated Blob enumeration a conditional metadata.
Expired recording se nejprve CAS-fencuje do terminalniho cleanup stavu se
starym updated_at, aby preruseny uklid mohl resume; metadata se nemazou pred
resource cleanup. Active continuous lease, legacy audio Blob lease nebo
finalizer lock zablokuje cely recording cleanup. Audio/chunk/raw/index deletes
nesmi implicitne obejit vlastnictvi. Po uspesnem uklidu se maze i cleanup lock.

Expired audit a orphan envelopes zachovavaji fixed completion expiry; invalid
orphan pouziva visible error a modified_at retention. Audit publication drzi
vlastni envelope lease do Table publikace. Cerstve unpublished audio se
nepovazuje automaticky za orphan jen kvuli dosud chybejici recording row.
To je zamerne konzervativni ochrana proti aktivnimu uploaderu; neznamy orphan
je odstranen po 48h objektove retenci, nikoli okamzite jako ve starsim cleanup.
Failed continuous source zustava do retence, ne do pouheho model failure.

Cloud binary voice-recorder ma api/worker/cleanup/check-config, finite HTTP/WS
probes, SIGTERM/Ctrl+C a explicitni startup errors. Startup necreateuje Azure
resources; kontroluje existujici containers, queue a Table metadata pres UAMI.
Readiness je true az po probes a bindu; shutdown ji shazuje a zrusi live relay
bez potvrzeni provisional textu. check-config je bez network/identity resource
volani; production fake/default-missing config failuje.

Source-only Rust Dockerfile zachovava api8000/worker/cleanup entrypoint, nonroot
UID10001 a OpenSSL CA trust. Dockerfile-specific ignore posila jen Rust source/
manifests a schvaleny Cargo config, ne user state, .git, target, C# outputs nebo
credentials. infra/deploy.ps1 ma explicitni BackendRuntime Rust switch, ale
Python zustava default. ACA API min1, KEDA worker min0, hourly Job, VNet/PE/UAMI
topologie se nemeni. Deployment script byl pouze parsovan, ne spusten.

Proslo 201 workspace testu (83 cloud library +2 CLI), locked release/fmt/Clippy
a existing Python audit/relay/SDK parity po publication-lease zmene.
Cleanup testy overuji expired/idempotent deletes, tri druhy active ownership,
fixed audit expiry/orphans a ochranu fresh audio. Prime REST testy overuji plnou
pagination/repeated-marker failure i read-only startup probes. Zadne Azure,
placene AI, real user state nebo resource changes se nekonaly.

Docker CLI29.6.2 je pritomny a rust:1.99-slim-bookworm manifest existuje, ale
lokalni daemon neni dostupny. Build image a nonroot/CLI guards jsou proto
zapojeny do CI bez push/deploy a zatim nejsou vydavany za uspesne overene.
Predchozi 885ca36 ma potvrzene Windows/Linux Rust i backend CI.
Full HTTP/OpenAPI/config edge parity, distributed fault cases, feature-matched
performance, MSRV a lidska real-world akceptace stale zbyvaji.

## Externi skutecny Rust HTTP versus FastAPI

Novy test-only cloud_contract example bezi na loopback portu, ma explicitni
memory stores a skutecny GoogleVerifier. Python probe generuje RSA key pouze
v pameti, servuje local JWKS a posila podepsany RS256 bearer; nepouziva
production auth bypass ani osobni tokeny. Stejny korpus posila do current
FastAPI factory s izolovanymi fixtures.

Porovnava actual HTTP status, Content-Type/Cache-Control/X-Request-Id/Allow/
WWW-Authenticate, RFC9457 body keys/error fields a recording/upload/complete,
audit summary/full/original/list tvary. Odhalil a opravil chybejici errors
pole, nested/missing/extra JSON field paths, pagination min/max status422,
digest/WAV field errors a automaticke JSON body media semantics. Rucne strict
dictation/refine/archive routes zustavaji oddelene. Message text obecnych
deserialize errors neni vydavan za byte-exact kopii Pydantic wording.

Proslo 202 workspace testu (84 cloud library +2 CLI), locked release/fmt/Clippy
a obe externi HTTP skupiny. Jde o bounded contract corpus, nikoli dukaz vsech
moznych JSON typovych koerci nebo fault permutations.
Cloud-container job predchoziho a2a1140 prosel skutecnym Linux image buildem
a nonroot UID10001/CLI/missing-config guards, bez image push nebo deploymentu.
Windows/Ubuntu Rust a backend CI take prosly. Lokalni Docker daemon zustava
nedostupny; acceptance prahy se tim nesnizuji.

## Konfigurace a obnovena native evidence

Konfigurovane preview/body limity nyni skutecne dosahuji audit/finalizer/stream
progress a upload readeru; default140 a8MiB zustava. Retention48h, batch1 a
PCM16mono16k se nesmi v migraci tise zmenit ani ignorovat. Nepodporovana override
konfigurace failuje explicitne pred resource access. CLI vypisuje safe detail
startup konfiguracni chyby, nikoli pouze generic status.

Cloud finalizer pri resume respektuje drive persisted preview limit, ne nove
nastaveni v okamziku retry. Nove prime testy overuji 8char audit preview,
64byte upload reader bound a invariant config rejection.
Proslo 205 workspace testu (87 cloud library +2 CLI), locked release/fmt/Clippy.

Actual C# reference build ma0 warnings/errors. Znovu proslo6 hidden native
lifecycle/Settings skupin,5 DPAPI/IPC/startup skupin a4 dictation text/checkpoint
skupiny, vse s explicitnimi syntetickymi soubory/namespaces. Aktualni installer
byl pouze sestaven, nebyl spusten ani instalovan:
SHA256 CF1FED76DF5246DA18F85C1D671BFA18F365DCB5FCCF908A718B005148BFA150.

Predchozi abf7f05 ma potvrzene Windows/Linux Rust CI, Linux container build/guards
a backend CI vcetne externiho HTTP korpusu. Rust1.85 neni lokalne nainstalovany;
novy MSRV check instaluje compiler pouze do disposable Windows/Linux CI runneru
a kontroluje locked all-targets workspace. Jeho verdict jeste neni znam.
Mereni kompletniho klienta proti feature-matched C# baseline a hodinovy capture
test nebyly provedeny a drivejsi foundation cisla je nesmi nahrazovat.

## Cleanup delete-failure a publisher fencing

Prime fault injection odhalila chybne poradi expired-transcript cleanup:
pri metadata delete pred selhavajicim Blob delete mohl zustat nedohledatelny
plain-text orphan. Nyni cleanup pod Blob lease odstrani expired body pred
conditional metadata; selhavajici delete ponecha metadata pro retry.
Old unpublished plain canonical bodies maji vlastni orphan sweep po48h
object retention, se second metadata check a active-publisher lease ochranou.

Finalizer drzi lease i na canonical body mezi jeho ctenim a Table publikaci.
Crash/cleanup tedy nemuze potichu potvrdit metadata k mezitim smazanemu body.
Renew/release vsech owned resources probehne i po partial error; silent
success-shaped fallback neni povolen.

Tri dalsi testy overuji failed body delete + metadata retry, partial recording
cleanup s durable tombstone/resume a old plain orphan versus active publisher.
Proslo 208 workspace testu (90 cloud library +2 CLI), locked release/fmt/Clippy.
MSRV1.85 all-targets checks prosly ve Windows i Linux CI na9619b20; jde o
compiler validation, ne tvrzeni o real-world device/production akceptaci.
Backend/container/Ubuntu CI tohoto commitu take prosly, Windows full conclusion
se overuje zvlast. Zadna lokalni toolchain instalace se nekonala.

## Synthetic native-to-cloud E2E

Tri Windows E2E testy nyni propojuji real NativeRecoveryStore + legacy/streaming
engines, BoundBatchBackend/ApiClient, Axum HTTP/WS router, MAI batch/stream REST/WS
adapters a audit endpoint. Audio/model/JWT vstupy jsou explicitne synteticke;
zadny mikrofon, clipboard, instalace, osobni store ani Azure/placeny model.
Native sample bytes jsou overene na model transportu i v zachovanem DPAPI
recovery po odhlaseni. Frozen original/final audit ma fixed48h expiry a zadny
normalni completion queue event.

Pouze Windows cloud dev-dependency aktivuje opt-in loopback-fixtures feature.
Factory povoluje jen explicitni http127.0.0.1:port bez credentials/query;
normalni ApiClient constructor stale vyzaduje HTTPS a system TLS trust.
Normalni native/installer dependency graph tuto feature neobsahuje. Nejde
o produkcni fallback, TLS bypass ani vypnuti cloud signature verifikace;
skutecny RS256 path zustava samostatne overen externim HTTP/JWKS probe.

E2E odhalilo, ze relay po finish prestal sledovat client disconnect a drzel
model/slot az do30s drain timeoutu. Nyni stale sleduje close; dalsi audio/control
po finish je odmitnute, potvrzeni se po chybe nevymysli a slot/upstream se
pri auth cancellation zavre promptne. Samostatny direct-socket test overuje
stejny close-after-finish scenar i na Linuxu.

Proslo 213 Windows workspace testu (94 cloud library +2 CLI,57 client library),
locked release/fmt/Clippy a externi actual Python relay/HTTP/audit parity.
Tri native E2E ani tento pocet nejsou feature-matched performance mereni.
Posledni build-only installer ma SHA256
02FC4F81057772371FE98BE963CAA75089AF562C6B9F36AD103347F797EE7BA8.
Predchozi deb0980 ma potvrzene Windows/Linux, obe MSRV1.85 kontroly,
container build a backend CI. Fyzicke Windows/Google/install/Azure testy a
baseline/hodinova acceptance jsou stale neprovedene lidske hranice.

## Aktualni acceptance checkpoint

Implementacni povrchy Windows/cloud jsou propojene a synthetic/direct/reference
checks jsou persistentne v CI. To neni schvaleni cutoveru ani tvrzeni o perfektni
celkove akceptaci. Nejnovejsi overeny code checkpoint 72a44c3 prosel Windows/
Linux Rust, obema MSRV1.85 jobs, container build/guards a backend parity CI.

| Check | Stav | Dukaz nebo presna mezera |
| --- | --- | --- |
| Implementace a bounded synthetic regressions | PASS | 213 Windows workspace testu, actual native/cloud E2E, C#/Python/reference parity; release/fmt/Clippy |
| Deklarovany compiler Windows/Linux | PASS | CI all-targets Rust1.85; bez lokalni toolchain instalace |
| Source-only Linux image | PASS | CI build, UID10001 a missing-config guards; zadny push/deploy |
| C01 feature-matched baseline | BLOCKED | Nutny vyhrazeny Windows profil/stroj bez osobni VoicePrompt instance; skutecny C# klient ma fixed IPC/per-user paths |
| C02 pamet/start/API/hour verdikt | NOT_RUN | Zbyva30 startu, vsechny predepsane serie a hodinova capture/idle stabilita; foundation cisla se nepouzivaji jako nahrada |
| C03 plna real-world akceptace | BLOCKED | Actual Google, mikrofon/focus/clipboard/paste, accessibility/DPI, install/upgrade a Azure private-endpoint/UAMI runtime vyzaduji vlastni scoped souhlas/prostredi |
| C04 lokalni uklid a neprodukcni hranice | PASS | Checkout/remote odpovidaji, fixture procesy a shell commands skoncily; zadna instalace, release, paid model nebo Azure zmena |

Pozadavek na izolovany Windows profil/stroj byl odeslan koordinujici relaci.
Samotne poskytnuti profilu neschvaluje instalaci, mikrofon, clipboard, Google,
Azure ani placene modely. Tyto akce se nesmeji provest bez dalsiho presneho
opravneni. Cely cil zustava nedokonceny na techto acceptance branach.

Dodatecny orphan safeguard omezuje novy plain-transcript sweep na znamy
canonical UUID.txt format. Nezname stare .txt objekty zustavaji nedotcene
s viditelnym diagnostickym zaznamem. Sedm cleanup testu vcetne unexpected-name
fixture a strict Clippy/fmt proslo; take finalni Windows/Linux/MSRV/container
a backend CI tohoto safeguard commitu jsou successful. Zadna produkcni data
nebyla ctena ani smazana.

## Installed startup parser regression

Kontrola startovaci composition cesty odhalila drive neotestovany wiring bug:
installed initialization byla omylem uvnitr vetve options s hodnotou. Obvykle
VoicePrompt.exe bez args nebo --installed proto nemusel vytvorit ClientOptions,
tedy ani zapojit compatible IPC a per-user klienta. Predchozi build-only installer
a negativni fixture testy tuto cestu samy o sobe neprokazovaly.

Parser je nyni samostatny bez I/O: installed/default packaged a isolated plany
jsou rozlisene pred dotykem state/IPC. Installed/quit rozhodnuti nezavisi na
poradi voleb nebo pritomnosti value argumentu. Installed mode zakazuje vsechny
fixture/value startup options, nikoli jen nektere. Packaged filename detection
je Windows-case-insensitive. Standardni uzivatelska installed inicializace
nasleduje az po hotovem planu a neprobiha v unattended testech.

Pet novych pure startup testu overuje default/explicit/quit, materializovany
installed client plan a konflikty v obou poradi. Process fixture overuje odmitnuti
mixed args a prejmenovane packaged EXE se self-test, bez aktivace osobni IPC
nebo zmeny user state. Proslo218 Windows workspace testu,6 platform a6 native
lifecycle parity skupin, fmt/Clippy a release. Installer byl jen rebuild:
SHA256 AB92DFB077270B2BD19206BAE7A09D99BF135ED1F978ACDC2AC1E555984975A2.
Skutecna instalace/start/upgrade zustava neprovedena; acceptance prahy se nemeni.

## Bezpecny full-client signed-out measurement fixture

Pro kontrolovany odhlaseny profil uz neni nutne spoustet osobni C# entry point:
minimalni VoicePromptClientFixture.exe vola skutecny WPF App.InitializeComponent/
OnStartup s internal fixture options. Normalni App konstruktor a produkcni
Create cesta zustavaji zachovane. Fixture meni jen explicitni state root,
oddeleny IPC namespace a zakazane physical ports; neni druhou implementaci UI
ani jen Core-only hostem. Actual tray, SystemEvents, auth/HTTP/realtime/history,
recovery, dictation/controller/overlay a audit lifecycle se inicializuji.

Rust --signed-out-fixture pouziva real run_tray/ClientHost a plny recovery/
dictation/audit runtime, ale bez OAuth/clipboard/device pristupu. Ma vlastni
--fixture-ready-file a scope full-client-signed-out-fixture; nesmi publikovat
foundation readiness nebo pouzit installed/ordinary explicit-state kombinaci.
Both fixtures odmitaji existing tokens, nespravny marker a reparse paths.
I pritomny synteticky OAuth env override je ignorovan; real config se nehleda.

Process tests overuji skutecne obe aplikacni composition cesty a bezpecny
graceful quit pouze vlastnich namespaces; input JSON zustava byte-exact.
C# build ma0 warnings/errors a402 Core regression testu proslo. Rust ma219
Windows workspace testu vcetne6 startup planning testu; fmt/Clippy/release prosly.
Novy fixture guard je zapojeny do Windows CI pres existing approved NuGet feed.

scripts/measure_clients.py sbira30 alternating startu kazde implementace a3
paired memory serie po301 samples. Pouziva stejny byte-identicky200-entry
corpus, kontroluje PID, scope, samplovani a binary/DLL bundle hashes pred/po.
Vyhodnocuje vsechny serie i explicitni80MiB/50% a1s/70% prahy; neredukuje je.
Smoke2/3/1 neni acceptance a ma sufficient=false. Full beh se provadi zvlast
az nad ulozenym code checkpointem a frozen binaries.

Tento profil nemeri signed-in networking, fyzicky mikrofon, clipboard/paste,
registry autostart, installed upgrade, cold OS cache ani hodinovou nahravku.
whole_migration_acceptance zustava false i pri uspesnem profilu. Puvodni
foundation mereni se nadale nesmi pouzit jako nahrada kompletniho klienta.

## Fixture CI diagnostika

Windows guard na631c11f prekrocil15s WPF startup budget, zatimco native,
Linux, oba MSRV a container jobs prosly; samostatny Windows build CI je successful.
Timeout se neprodluzuje bez zjistene priciny. C# fixture ma opt-in vlastni
phase-progress soubor u synthetic ready markeru; guard ho pri selhani spolu
se stderr zachova ve vystupu testu. Produkcni startup ani measurement default
diagnostiku nezapinaji. Lokalni aktualni C# build ma0 warnings/errors a oba
actual full-client fixture guard testy prosly.

Kontrolovane signed-out mereni frozen631c11f stale bezi: vsech30+30 startu
a prvni paired301-point memory serie jsou sebrany. Partial data nejsou finalni
C02 verdict; zbyvajici serie, CI pricina a ostatni acceptance brany trvaji.

## Linux authenticated protocol performance fixture

Novy kontrolovany profil spousti actual FastAPI factory pres Uvicorn a actual
Axum router pres existing cloud_contract example. Obe strany pouzivaji real
RS256/JWKS verifier se stejnou generovanou autoritou, nikoli StaticTokenVerifier.
No-token, forged signature a validne podepsany token s nepovolenym e-mailem
musi byt odmitnuty.
Pouzivaji se vyhradne loopback endpoints, memory data ports a identicke vstupy;
model/refinement/realtime endpoints jsou na obou stranach explicitne unavailable.
Nespousti se worker, cleanup, Azure probes, managed identity ani placeny model.
Normalni aplikacni EXE nejsou pripustnym fixture binary.

scripts/measure_backend.py meri30 alternating startu az po HTTP readiness a3
paired serie po100 flows/700 operacich. Create/get/upload/complete/archive/
get-transcript/list overuji i skuteckou ulozenou odpoved a fixed48h audit.
RSS se cte pouze z vlastnich Linux PID po kazde serii; jde o kratke post-load
pozorovani, nikoli petiminutovy idle nebo hodinovy test. Meri se vsechny serie/
operace;50% RSS/start a110% API prahy se nesnizuji. Nezmeneny source/binary/lock
bundle, presne pocty vzorku, nearest-rank p95 a atomic no-overwrite publikace
jsou kontrolovany. Kazdy raw sample je v CI artifactu.

Pet local safeguards/actual HTTP factory testu je zapojeno do backend-ci,
ktery zachovava approved locked Python feed. Linux collector se teprve provede
v tomto disposable runneru; lokalni Docker daemon ani Linux host neni dostupny.
Ani uspesny profile verdict neprokazuje plneho production API RSS/readiness,
ACA0.5vCPU limity, private Azure latenci, worker nebo AI/relay. Proto ma vysledek
scope authenticated-in-memory-http-no-ai-realtime a whole_migration_acceptance=false.
Neni to feature-matched kompletni cloud C01/C02 ani nahrazeni jejich dosud
chybejicich mereni a lidskych hranic.

## Dokoncena kontrolovana mereni a timer rotation

Full signed-out klientsky profil na frozen631c11f dokoncil vsech30+30 startu,
tri paired serie po301 jednosekundovych vzorcich a stejnych200 history entries.
Nezavisly prepocet overil vsech1806 memory samples, source/scope/PID/binary
identitu, corpus hashes, skutecne petiminutove intervaly a nezmenene C# DLL/
native bundle. Vsechny own fixture procesy graceful skoncily a state roots
byly odstraneny; osobni aplikace/data ani physical ports se nepouzily.

| Kontrolovany Windows signed-out profil | C# | Rust |
| --- | --- | --- |
| Process -> full-runtime ready, p95 | 1476.585ms | 446.809ms |
| Mean private bytes, MiB | 94.374 | 3.426 |
| Peak private bytes, MiB | 107.078 | 3.836 |

Rust p95 je30.26% baseline; mean private bytes v jednotlivych paired seriich
jsou3.72%,3.70%,3.47% C#. Vsechny serie tedy splnily presne1s/70% a80MiB/50%
prahy tohoto profilu. Private bytes nejsou RSS ani working set.
Evidence: session artifact client-comparison-full-631c11f,
full-client-signed-out-comparison.json SHA256
01fed312ceb8f2779d804b9c568c7d2b1e392f0defa0e0e88c25b381bc9b445d.

Linux protocol profil na250ecc2 prosel v backend-ci41385009835. Actual
FastAPI/Uvicorn pouziva stejny auto-selected event-loop startup jako produkcni
entry point, nikoli zpomalenou asyncio.run nahradu. Nezavisly prepocet vsech
raw metrik a hashes proti presnym Git source blobs potvrdil30+30 startu,
tri serie po100 flows/700 operacich na implementaci a60 own-PID RSS pozorovani.

| Linux authenticated memory-port HTTP profil | Python | Rust |
| --- | --- | --- |
| Process -> HTTP readiness, p95 | 610.409ms | 36.985ms |
| RSS po serii1, MiB | 103.746 | 16.316 |
| RSS po serii2, MiB | 104.242 | 18.590 |
| RSS po serii3, MiB | 104.766 | 20.777 |

Rust startup je6.06% baseline; vsechny RSS serie jsou15.73-19.83% baseline.
Nejhorsi pomer vybrane API p95 je67.74% baseline; zadna z21 operation/series
kombinaci neprekrocila110%. Artifact backend-protocol-performance,
backend-protocol-performance.json SHA256
2a2c8cec2ac6413dc8c71f57198b8183291c67943040a52dee96a5d5987d17e9.
Podminky/omezeni tohoto memory-port profilu zustavaji uvedene vyse; nejde o
production-adapter, ACA, worker nebo AI/relay performance.

WPF guard s diagnostikou na d82520d prosel celym Windows/Linux/MSRV/container
CI. Puvodni15s timeout se zatim nepodarilo reprodukovat ani urcit jeho pricinu;
nebyl proto zamaskovan prodlouzenim budgetu nebo odstraneny z evidence.
Aktualni installer byl pouze sestaven, SHA256
B0007D8A469FA38F4798295CC461EDBE4CBE1B702FCEB4D427D68C7C0042F467.
Nebyl spusten ani instalovan.

Novy native test s virtualnim Tokio clock overuje50min session rotation:
capture zustava aktivni, novy stream zacina na durable confirmed cursoru,
potvrzene audio se neopakuje a dalsi odlisne sample bytes se spravne dokonci.
Test-util feature pouziva existujici locked Tokio1.54.1 pouze jako Windows
dev-dependency; normalni installer graph ji neobsahuje. Proslo220 Windows
workspace testu a strict fmt/Clippy. Toto neni realna50min/hodinova nahravka
ani memory-growth measurement.

Celkovy C01/C02 zustava castecnym dukazem, ne PASS kompletni migrace:
signed-in traffic, mikrofon readiness, hodinovy capture/return-to-idle,
physical focus/clipboard/paste, pristupnost/DPI, skutecny installer upgrade/
rollback a private Azure/UAMI runtime jeste nebyly akceptovany. Oba reporty
maji nadale whole_migration_acceptance=false. Zadny threshold se nesnizil.

## Oddelene functional-CI a measurement budgety

9e01689 CI znovu zachytilo C# fixture15s timeout, tentokrat s phase evidence:
validace probehla04:13:06, WPF Application konstrukce04:13:07.703,
resources04:13:14.995 a startup/instance04:13:16.330. Samotne resources tedy
trvaly7.292s a postup pokracoval; nebyl to dukaz zaseknuteho auth/network/
dictation enginu ani missing framework. Presnou OS pricinu pomaleho cold
loaderu tento trace neurcuje. Vsechny Rust testy, Linux/MSRV a image jobs prosly.

Functional isolation guard omylem sdilel15s merici safety budget s prvnim
cold WPF startupem na disposable CI stroji. Nyni ma pouze tento C# functional
test explicitni bounded60s setup budget; native guard zustava15s. Trace a
stderr se pri chybe stale zachovaji a uspech loguje skutecne elapsed startup.
Measurement collector nadale pouziva default15s a presne1s/70% a80MiB/50%
akceptacni prahy. Zadny z techto prahu ani30/3/301 sample count se nemenil.
Novy test overuje zachovany default a odmitnuti neomezeneho/invalid budgetu
pred spustenim procesu nebo zapisovanim fixture stavu.

Tri local actual-client guard testy prosly. Meni se pouze functional test setup;
produkcni runtime, measurement defaults a namerena cisla zustavaji stejna.
C# rollback baseline nema
Rust1s startup cil a functional cold-VM guard neni performance acceptance.

Code checkpoint b75f367 ma nyni successful vsechny Windows/Linux Rust,
MSRV1.85 a non-root container jobs:
[rust-ci41387511745](https://github.com/tkubica12/cot-voice-recorder/actions/runs/41387511745).
Actual CI guard zaznamenal C#3462.307ms a native70.583ms, vsechny tri isolation/
budget testy prosly; virtual50min rotation take prosla. Tato dve CI startup
pozorovani nejsou nova C01/C02 serie ani nahrada ulozenych30-start profilu.

Povoleny lokalni/source inkrement je commitnuty a pushnuty. Potreba vyhrazeneho
Windows test prostredi a oddelenych opravneni pro real-world testy byla predana
koordinatorovi. Kompletni migrace neni oznacena jako hotova; zadny skutecky
Google/device/clipboard/install/Azure test nebo produkcni zmena se tim neschvaluje.

## Pokracovani: queued capture retention a skutecny synthetic hour

Native controller review odhalilo konkretny datovy bug: po auth change,
Interrupt nebo selhani microphone stop se preserve flag pouzil take jako zakaz
ulozeni jiz prijatych queued PCM buffers. C# consumer pritom prestava ukladat
jen pri explicitnim discard. Native nyni draine vsechny accepted buffers do
stejneho recovery journalu i v preserve vetvi; samotne preserve stale zakazuje
delivery. Explicitni discard neuklada nic, prvni storage error zustava viditelny
a dalsi write se po nem nepokouseji. Tri testy overuji realnou DPAPI v3/v4
persistence, presne obnovene sample bytes/context, discard a storage failure.

Novy explicitni ignored libtest fixture meri realny cas, nikoli virtual clock:
2s warmup, stejny empty-recovery idle pred/po,20ms deterministic PCM cadence,
16000 samples/s a actual StreamingSession/NativeRecoveryStore s default256MiB
kvotou. Provider kontroluje kazdy sample a absolute cursory bez uchovavani
kopie cele nahravky; durable audio se uvolnuje jen po typed confirmed cursoru.
Full3600s run musi prokazat115200000 exact PCM bytes a skutecny50min rotation.
Nevynucuje se GC/heap trim a zadny audio limit/kvota se nesnizuje.

scripts/measure_synthetic_capture.py uklada before/capture/after raw samples,
actual elapsed time, PID/binary hashes a maximum idle growth proti warm
baseline. Full run pouziva301/3601/301 jednosekundovych bodu a konzervativni
10MiB check worst post-idle minus median pre-idle.3s/3-point smoke probe
prosla a ma sufficient=false. CI spousti pouze tento bounded smoke; ignored
hour neni implicitne zapocitany jako PASS.223 normalnich Windows Rust testu
a strict fmt/Clippy proslo; jeden long fixture zustava explicitne ignored.

Je to engine-only profile bez GUI/controller, microphone, system clipboard,
auth, skutecneho cloud/model transportu nebo osobnich dat. Report ma
full_client_profile=false a whole_migration_acceptance=false i pri PASS.
Nenahrazuje kompletniho signed-in klienta, microphone readiness ani fyzicke
akceptacni brany. Skutecny hodinovy beh se zahaji oddelene nad frozen binary
po ulozeni tohoto source checkpointu; smoke nenahrazuje jeho chybejici data.

## Cancellation ordering a bounded native delivery

Dalsi synthetic fault review odhalilo, ze Escape zrusilo jen Paste uz prijaty
UI, nikoli Deliver cekajici ve fronte nebo teprve odesilany background actorem.
UI nyni okamzite latchne cancellation pro aktualni generation, zrusi existujici
token a odmitne vsechny late Deliver. Stale zustava busy do terminalniho stavu
actora, takze nelze spustit druhy engine; late Recording/Finishing status uz
nezobrazi zruseny overlay. Opakovany Escape nezaplni command queue.

Native actor mel3s timeout pouze na UI odpoved, zatimco ui.send mohl cekat
neomezene na plnou frontu a nepozoroval auth/discard/shutdown.3s limit nyni
zahrnuje enqueue i acknowledgement a stejny select sleduje cancellation po
celou dobu. Timeout/closed queue je viditelna chyba, History zustava fallback
a CancelOnDrop invaliduje token drive, nez muze late UI zapsat clipboard.

Delivery/audit policy je explicitne inactive uz pred spustenim runtime threadu.
Spawned outbox task tak nema startup okno se zdedenym active default pred prvni
auth publikaci; normalni signed-in stav ji pozdeji zapne stejnou cestou jako driv.

Dva hidden native UI testy overuji Escape/late delivery/overlay bez system
clipboard nebo SendInput; tri virtual-clock/channel testy overuji full queue,
late reply a closed receiver; actual isolated ClientHost test overuje signed-out
startup policy bez OAuth/network/device/autostart. Proslo229 regularnich Windows
Rust testu a strict fmt/Clippy; jeden hour fixture zustava ignored v beznem runu.
Frozen09bc686 real-hour engine-only mereni bezi oddelene a neni temito buildy
meneno. Zadne z teto coverage nenahrazuje stale nepovolene fyzicke testy.

## Upload header error parity

Rozsirena actual FastAPI versus Rust HTTP matice odhalila10 konkretnich
odchylek: invalid optional upload headers vracely chybejici RFC9457 errors
nebo byly prekryte driv provedenou WAV/body validaci. Nyni Rust sesbira path
index a vsech pet optional header errors ve stejnem poradi jako framework
reference, zachova header alias ve field path a vrati je pred ctenim body.
Validni upload data/queue/storage flow zustava stejne.

Deset single-header + WAV/bad-body pripadu a combined invalid path/headers/
media pripad prosly proti actual referenci. Dalsi prime Axum test pouziva
sledovany streaming body a prokazuje, ze pri chybne metadata validaci nebyl
ani pollnuty. Proslo230 regularnich Windows Rust testu (95 cloud library),
strict fmt/Clippy a vsechny tri externi HTTP parity groups. Nejde o tvrzeni
uplne shody vsech Pydantic coercions nebo kazdeho message wording.

## Physical-port boundary a upresnena autorita hostu

Posledni UI boundary nyni kontroluje take explicitni physical-enabled flag,
nejen auth/cancel/busy stav. Ani omylem queued authenticated Deliver tedy nesmi
v isolated/fixture modu pristoupit k systemovemu clipboardu nebo SendInput.
Regresni test overuje pure admission predicate nad hidden owned UI; nevola
skutecny clipboard ani kdyby samotny predicate v budoucnu selhal.

0790ef7 ma successful full Rust Windows/Linux/MSRV/container a backend CI.
Refreshed30+30/3-series authenticated memory-port HTTP profil ma p95 readiness
Python576.258ms vs Rust36.263ms, paired RSS Python103.703/104.191/104.699MiB
vs Rust16.078/17.984/19.949MiB a nejhorsi API p95 pomer77.66% baseline.
Vsechny scoped prahy znovu prosly; full production acceptance zustava false.
Artifact SHA25619236e4919e5eee1488b4dc1f8e4590455e1ebc58a37a0ba771d88db0a8d06d6.

Koordinator predal drivejsi vyslovne rozhodnuti uzivatele: tento pocitac je pro
testy schvaleny a uzivatel uvadi, ze osobni VoicePrompt instanci vypnul.
Jiny Windows profil/stroj tedy neni dalsi organizacni podminka, pokud lze
konkretni test bezpecne izolovat explicitnim synthetic rootem a IPC. Pred
relevantnim testem je stale nutne overit skutecny stav. Osobni soubory/cache/
tokeny/recovery se neoteviraji.

Toto rozhodnuti neschvaluje microphone capture, system clipboard/paste,
Google login, skutecny install/upgrade, Azure ani placene AI. Po aktivnim
hodinovem engine checku se koordinatorovi preda nejmensi konkretni fyzicky
test a jeho skutecna akce/dusledek, nikoli dalsi obecna zadost o jiny profil.

Pro tento nejmensi dalsi krok je pouze zkompilovana explicitne ignored native
probe: manual_default_device_240ms_count_only_no_store. Pred jakymkoli otevrenim
zarizeni vyzaduje oddeleny explicitni opt-in; samotny env flag nenahrazuje
souhlas uzivatele. Po pripadnem souhlasu otevre default WinMM vstup na240ms,
jen spocita PCM bytes/buffers, zeroizuje je, provede reset/close a vypise timing/
status. Nic neuklada/neuploaduje a nema auth, UI, hotkeys nebo clipboard cestu.
Zatim NEBYLA spustena. Existing C# --microphone reference ma stejny240ms
count-and-clear profil a take nebyla spustena.41 native nonphysical testu
proslo; hour a manual microphone fixture jsou dve zamerne ignored polozky,
nikoli zapoctene PASS fyzicke akceptace.

Full-client signed-out collector nyni fingerprintuje take NAudio Core/WinMM
DLL, oba application deps/runtimeconfig soubory a explicitni app icon, nejen
hlavni apphost/managed DLL. Pure synthetic-file test prokazuje, ze zmena runtime
dependency zmeni attestation, zatimco unrelated OAuth/config soubor se nehleda.
Vsechny ctyri full-client guard tests prosly.

Aktualni release client bundles0a2e715 jsou allowlist-copy frozen pro dalsi
30+30/3-series signed-out recheck po dokoncenych opravach. Rerun se nezahajuje
soubezne s aktivnim CPU-heavy hour capture, aby agent sam nezmenil zatez mezi
baseline/target phases. Namerena631c11f data zustavaji pravdiva pro svuj source
checkpoint; nejsou automaticky vydavana za novou verzi po dalsich opravach.

## Hour01 neuspech a linearni confirmed-audio cleanup

Skutecny09bc686 hour beh selhal:301 warm-idle a3601 capture samples se ulozily,
ale engine nedodal completion/idle marker v navazujicim30s okne. After-idle
data ani success report nevznikly. CPU postupne rostlo az k93% jednoho jadra;
private bytes na konci capture byly7.22MiB. Tato neuplna data nejsou PASS
hour/memory-growth akceptace a zustavaji zachovana jako failure evidence.

Review odhalilo konkretni quadratic filesystem praci: kazde nove streaming
confirmation vracelo vsechny dosud potvrzene indexy. Native journal tak znovu
guardoval/deleteoval tisice uz davno smazanych souboru. Nyni uvolnuje pouze
nove potvrzene chunky. Samostatny reopen pass explicitne retryuje confirmed
orphan po publish-before-delete crashi; nikdy neodstrani pending audio.
Disk manifest se stale znovu cte pod sdilenym gate, CurrentUser DPAPI/entropy,
quota256MiB, v1-v4 format, absolute cursor a confirmation pravidla se nemeni.

1000-confirmation test prokazuje presne1000 released indexu, nikoli soucet
1+...+1000. Actual native v3/v4 file-lock/crash-orphan test prokazuje viditelnou
chybu, retry po reopen a zachovane neconfirmovane sample bytes.233 regularnich
Windows Rust testu, strict fmt/Clippy a sedm actual C#/Rust recovery/DPAPI
parity groups proslo. Rozsireny accelerated115200000-byte synthetic volume
probe prosel vsemi1800 durable checkpointy a kazdym sample za351.827s;
je oznacen real_hour=false a NENAHRAZUJE nove wall-clock mereni.

Collector ted kontinualne draine bounded stdout/stderr, aby error spam
nezablokoval dite. Pri chybe uklada status=failed, logs, minute progress/
phase markers a kopii pouze vlastniho synthetic state. Zero-test a real
child-pipe-backpressure failure testy prosly; neuspech nemuze publikovat
complete report. Full hour se musi znovu provest nad novym frozen buildem,
se stejnymi301/3601/301 samples a nezmenenym5s completion/30s controller oknem.
Fyzicky mikrofon, clipboard, Google, install ani Azure se zatim nepouzily.

Publish-before-delete cleanup retry zustava take na stejnem otevrenem journalu:
identicke confirmation na uz durable cursoru explicitne retryuje leftovers,
zatimco normalni postupujici cursor stale vraci jen nove potvrzene indexy.
Rozsireny actual v4 file-lock test nejprve prokaze viditelnou chybu, pak
same-cursor retry, nasledne samostatny crash/reopen retry; pending audio
zustava zachovane v kazde fazi. Vsech devet native store testu a strict
Clippy/fmt proslo. Prave bezici25f0e21 frozen hour retry se nemeni.

## Preruseni hostu a restart-resilient acceptance

Po resume08:24 nebyla predchozi tool session dostupna a owned native28640
ani collector uz nebezely. Posledni minute checkpoint mel1560s/49920000 PCM
bytes; nevznikl capture sampler report, engine completion ani after-idle.
Exact zdroj ukonceni nelze z techto dat prokazat. Run25f0e21 je proto
incomplete_interrupted, nikoli novy engine PASS/FAIL; originalni explicitni
state a interruption.json zustaly v session artifacts zachovane.

Sourcef572599 uz ma successful Windows/Linux/MSRV/container CI vcetne
linear release, same-cursor/crash retry, short collector smoke a accelerated
volume validation. Aktualni release fixture byl znovu frozen,3s smoke a oba
failure/pipe guards prosly. Nova bounded shell chain durable-acceptance-f572599
je explicitne detached, protoze musi prezit prave pozorovany zanik app/tool
procesu; nejde o nekonecnou sluzbu, novou session nebo scheduled automatizaci.
Provadi real hour301/3601/301 a jen po uspechu nove signed-out30+30/3x301
porovnani f572599 bundle. Dva profily jsou sekvencni, ne soubezne; nepridavaji
physical/auth/clipboard/Azure ani installed vstup.

Dosavadni nekompletni hour runs se neprepisuji ani nevykazuji jako uspech.
Vysledky nove chain se overi z raw evidence a skuteckych procesnich identit,
nikoli z pouheho faktu, ze command byl odstartovan. Celkovy migracni accepted
status zustava false, dokud nejsou dokonceny zbyle povolene i lidske brany.
