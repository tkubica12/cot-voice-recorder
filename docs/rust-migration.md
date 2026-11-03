# Migrace Windows a cloudove casti do Rustu

## Rozhodnuti a rozsah

Postupna migrace do Rustu, nejdrive Windows a potom cloud. Nejde o jednorazovy
prepis: nejprve overit prinos na funkcnim pilotu, potom prenaset chovani a data.
Android, Azure sluzby a Bicep zustavaji zachovany. Posledni rozhodnuti uzivatele:
zustat u klasicke ACA; na ACA Express se v teto migraci neprechazi.

Uzivatel nasledne vyslovne doplnil zavaznou vizualni podminku:
"at to ma i puvodni ikonku a velmi podobne zive UI! nejen identickou
funkcnost. Nastavovaci menu appky muze byt designove jine, ale UI popup
apod. bych rad co nejpodobnejsi".
Puvodni repo assets/ikona se musi znovu pouzit na vsech relevantnich
native EXE/tray/window/shortcut/installer povrsich. Dictation overlay,
popupy a stavove indikace maji vychazet ze skutecneho puvodniho WPF
UI/source vzhledem, interakcemi a zivymi/reaktivnimi prechody, ne z noveho
domysleneho stylu. Settings smi byt designove jine. Funkcni/data parity
bez vizualniho overeni tuto novou akceptacni podminku NESPLNUJE.
Original icon resource parity a prvni actual native/WPF reactive-overlay
vizualni srovnani jsou nyni overene v nize vymezenem synthetic scope.
Cele UI/physical/DPI/accessibility acceptance tim neni uzavrene.
Drive namerene performance dukazy nad puvodnimi
binary se neprepisuji na budoucni UI zmeny; relevantni regrese se zmeri
znovu v odpovidajicim scope.

## Aktualni progress

| Cast | Stav |
| --- | --- |
| Plan, vetev a prvni commity | Ulozeno a pushnuto |
| Audio jadro a C#/Python parity | Implementovano a overeno |
| Zdroj externich crates | crates.io vyslovne schvalene jako projektova vyjimka |
| Nativni Windows zaklad | Tray, izolovana instance a DPAPI implementovany; lokalni self-test a C# DPAPI parity prosly |
| Mereni Windows klienta | NOVA vyslovne schvalena quiet-window current071/checkout86f3b0f full-runtime signed-out30+30/3paired301-point PASS: p95Rust257.948ms,meanprivate3.427MiB,observedpeak3.992MiB. Vsechnyrawvalues/pairedthresholds/cleanup nezavisleovereny;starsi7bPASS ia2d/3f4FAILEDINCOMPLETE zachovany,neprepisuji se na signed-in/physical scope |
| Microphone readiness | Jeden actual240ms probe a samostatne schvalena30+30 paired serie prosly; first-PCM p95Rust80.75% C#baseline, bez audio souboru/uploadu |
| Engine longevity | Actual3600s synthetic PCM/real50min rotation,115200000 exact bytes; worst idle growth1.645MiB<10MiB, engine-only PASS |
| CI nativniho inkrementu | 7b4aff4 original-icon/reactive-overlay Windows/Linux/MSRV1.85/container RustCI a C#WindowsCI PASS; nasledne observer/input inkrementy maji vlastni CI, nejde o real-world cutover |
| OAuth/token-store/PubSub klient | Native flow implementovany; samostatny live Google auth-only/local-signout a assisted manual clipboard probe PASS, full signed-in backend/controller dictation stale chybi; samostatny automatic DesktopGuard port viz dalsi radek |
| Automatic DesktopGuard paste port | NOVYexactonephysicalpostfix5b test PASS:one4eventnativeCtrlV/exactownfieldmarker/normalclosed,no retry. Nejdenifullcontroller/hold-toggle/hour/realbackend acceptance;older50040BLOCKED/45788assisted zustavaji |
| Diktovani/recovery UI | Native microphone/hotkeys/overlay/paste controller, oba enginy, journal, polishing a outbox implementovany; real-world a rozsahlejsi fault/E2E akceptace zbyva |
| History cache | JSON/retence/200 entries, atomicka persistence, native list/text a manual copy latest/selected/original implementovany; C# file parity overena |
| Settings a instalator | Vsechna C# JSON pole, nativni Settings/History/Recovery, DPI fonty, per-user autostart a kompatibilni IPC; installer sestaven, nebyl instalovan |
| Original icon a podobne zive popup UI | Original ICO embedded/byte-exact a DPI sizes overeny; WPF-matched reactive overlay15 raster pairs a noactivate/900ms proof, actual144DPI original-themed main/History/Recovery/default/min/lazy presentation overeny. Human keyboard/screen-reader/monitor/physical akceptace stale zbyva |
| Rust cloud API, worker, cleanup | API/AI/PubSub/live relay, processors/finalization/queue loop, cleanup, bootstrap/CLI a source-only image wiring implementovany; image CI proslo, uplna akceptace zbyva |
| Cloud adapter-matched performance | 33d04aa actual verified-HTTPS Azurite RustREST/PythonSDK30+30/3x100flows PASS po nezavislem prepocitani; neni actual Azure IAM/PE/ACA/model ani worker-latency acceptance |
| Hosting | Klasicka ACA zustava: API, KEDA queue worker a hourly cleanup Job; Express byl uzivatelem zrusen |

Puvodni foundation mereni neprokazuje kompletniho klienta a nelze je pouzit
jako nahradu nyni explicitne vymezeneho full-runtime signed-out profilu.
Ani jeho PASS sam o sobe neprokazuje full signed-in/physical/cloud akceptaci.

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

## Dokonceny actual synthetic engine hour f572599

Restart-resilient engine beh f572599 dokoncil skutecnych3600.003454s,
115200000 sample-exact captured/provider/confirmed PCM bytes a realny
stream rotation na absolute cursoru95872000. Vsech2620 checkpointu bylo
potvrzenych, session shutdown joined a store se vratil do stejneho prazdneho
recovery idle. Nezavisle bylo overeno vsech4203 raw samples,301/3601/301
pocty a casy, vlastni PID/image hash i kompletni finalni metriky.

| Synthetic native engine hour | Vysledek |
| --- | --- |
| Mean private warm idle | 2.045MiB |
| Mean private po shutdown/idle | 3.547MiB |
| Mean idle growth | 1.502MiB |
| Worst post-idle minus median warm baseline | 1.645MiB; limit10MiB PASS |
| Peak capture private | 6.410MiB |
| CPU one-core mean pred/behem/po | 1.82% /9.27% /3.19% |

Artifact synthetic-engine-hour-durable-f572599/synthetic-engine-hour.json
SHA256 ceeeb07a5bea2cb514fcbf26897752f5061e785266f377d773b83c2f7c26a430.
Producer mel na sdilenem stroji maximum scheduling lateness18.195s; presne
sample poradi/pocty zustaly zachovane. Neni to mikrofon/dropout/real-model
nebo full-GUI-controller akceptace. Report nadale ma full_client_profile=false
a whole_migration_acceptance=false. Native hour proces skoncil a veskery
generovany hour state byl uklizen.

Navic skutecny preruseny26min journal byl na oddelenych kopiich otevren
actual Rust i C# RecoveryStore:50368000 captured,50304000 confirmed bytes,
1114 checkpointu a jeden pending. Oba ctenari vratili shodny stav a pending
audio zustalo validni; originalni tri soubory zustaly hash-exact nedotcene.
To prokazuje real interrupted-data rollback, ne dodatecny hour PASS.

Navazujici signed-out rerun po sesti startup parech narazil na PermissionError
pri cteni prave vznikleho ready markeru. Partial startup data nejsou uplne
mereni a maji vlastni failure evidence. Oba published writers zaviraji file
pred atomic rename; konkretni transient Windows lock owner neni prokazan.
Collector nyni povazuje za ready az uspesne bounded cteni, nikoli exists().
Jen FileNotFound/PermissionError maji kratky retry, PermissionError maximalne
250ms v ramci puvodniho15s startup deadline; ten ani1s/70% cile se nemeni.
Do latence se zapocita cely wait a trial se nerestartuje. Persistentni ACL/
access error, oversize nebo invalid JSON stale explicitne selze.
Sest guard tests vcetne simulated transient/persistent lock a actual frozen
client composition proslo; novy uplny profil se zmeri znovu zvlast.

## Jeden vyslovne schvaleny physical microphone check

Uzivatel povolil presne jeden Rust default-device240ms count-only/no-store
test. Prvni read-only preflight nasel znovu bezici installed VoicePromptPID31964,
takze test body tehdy nebyl spusten a osobni aplikace se neukoncovala. Po
dalsim uzivatelskem potvrzeni, ze VoicePrompt zavrel, preflight overil jeho
nepritomnost a probe se spustila presnym ignored-test filtrem pouze jednou.

Vysledek08:53:00Z:6 buffers,6978 PCM bytes spoctenych a zeroizovanych,
default-input open603.7545ms, stop26.0579ms. Native finish/reset/unprepare/
close cesta nevypsala zadnou chybu, capture worker byl joined a probePID7808
skoncil exit0 se stderr prazdnym. Nebyl ulozen/uploadovan zadny zvuk,
neoteviral se auth/cloud, clipboard/paste ani hotkeys. Frozen probe SHA256
21788E86E998CE5D54B38F95726C0240AD7EB483688F3EBF03DBA622A018CDE6;
exact scoped evidence je v session approved-single-test-result.json.

Souhlas je spotrebovany. Zadne opakovani nebo C# microphone baseline nebyly
provedeny ani povoleny. Tento jeden actual device check nema predstavovat
30-trial readiness/110%baseline verdict, jine zarizeni, full-client hour nebo
focus/paste akceptaci. Tyto zbyle scenare maji stale vlastni evidence/permission
hranice. Bezpecne signed-out mereni s corrected readable markerem probiha zvlast.

Pro pripadnou dalsi, dosud NEPOVOLENOU readiness serii jsou pouze zkompilovane
count-only probes doplnene o cas prvniho validniho PCM bufferu. Samotny
StartRecording/open return neni dostatecny proxy pro skutecnou PCM readiness:
Rust meri prvni prijem z vlastni bounded capture fronty, C# prvni DataAvailable.
Obe nadale pouzivaji240ms count/zero a zadne audio soubory, upload nebo clipboard.
C# output navic uvadi open/stop/dispose timing a pocty; native telemetry pridava
first_pcm_ms. Puvodni jediny schvaleny vysledek z frozen probe zustava pravdivy,
ale first-PCM cas nemeril. Zadna z techto nove instrumentovanych physical
probe nebyla spustena; C# build ma0 warnings/errors, Rust Clippy/fmt a presne
jeden synthetic queue test prosly. Repeat/paired baseline vyzaduje novy
konkretni souhlas, nejde o implicitni rozsireni jedineho predchoziho testu.

Uzivatel nasledne samostatne schvalil presne jednu alternating30Rust+30C#
serii, kazdy trial240ms count/zero a zadne audio soubory/upload, auth/cloud,
clipboard/paste/hotkeys. Collector scripts/measure_microphone.py je bounded na
presne60 pozorovani a nema automaticky retry. Pred otevrenim zarizeni overi
nepritomnost osobni/physical VoicePrompt instance a explicitni image names;
hashuje actual managed/runtime DLL bundle i nativni fixture. Uklada pouze
typed pocty/timing/PID/exit a nejprve attempt marker, aby selhani nebylo
maskovano opakovanim. Vyhodnocuje vsech30 trialu nearest-rank p95 first-valid-
PCM proti puvodnimu110%baseline cilu; open-return je jen vedlejsi telemetry.

Ctyri pure synthetic telemetry/schema/completeness/threshold testy prosly
a CI spousti pouze je, nikdy fyzicky collector. Ulozena schema neumoznuji
vydavat chybejici/invalid first_pcm_ms nebo nekompletni trial count za uspech.
Samotny CLI opt-in flag neni opravneni; konkretni uzivatelsky souhlas je
nutnou predchozi podminkou provadene serie.

## Physical paired microphone readiness: schvalena serie dokoncena

Jedna samostatne schvalena serie probehla presne30Rust+30C# trialu v alternating
poradi, kazdy s240ms pozadovanym capture oknem, bez jedineho automatic retry.
Vsech60 attempt/result records, actual frozen bundle hashes, typed PCM counts,
process exit0 a raw timing vypocty byly nezavisle overeny.

| Default-device readiness, p95 | C# | Rust |
| --- | --- | --- |
| Prvni validni PCM, primarni criterion | 352.8143ms | 284.8815ms |
| Open return, jen vedlejsi telemetry | 276.0153ms | 213.0476ms |
| Stop/dispose | 21.8863ms | 33.4994ms |

Rust first-PCM p95 je80.7455%baseline a splnuje puvodni<=110% cil. Neni to
open-return proxy; prvni PCM je v C# skutecny DataAvailable a v Rustu validni
buffer prijaty z real capture fronty. Stop je zvlast uvedeny a neni zamaskovan
jen rychlejsi first-PCM metrikou. Nezavadel se novy stop benchmark threshold.

Vsech60 vlastnich procesu skoncilo; capture/close nemelo zadnou stderr chybu.
Neulozil/neuploadoval se zadny zvuk, buffer byl RAM-only/zero a nepouzil se
auth/cloud, clipboard/paste ani hotkeys. Osobni instance byla nepritomna
pri preflightu a agent ji neukoncoval. Report
paired-microphone-readiness-cc3367b/microphone-readiness.json ma SHA256
50ca8fe94a7959293e95f7d27c7cba1c443d8a1f55eb999111786e422817aaf1.

Tento druhy konkretni souhlas je spotrebovany. Serie nemeri jine vstupy,
hotkey/focus/paste ani full-client-hour/network chovani. Proto zustava
whole_migration_acceptance=false; dalsi fyzicke scenare nelze automaticky
zopakovat nebo pripojit k drivejsimu mikrofonimu opravneni.

## Aktualni full-runtime signed-out client recheck

Corrected readable-readiness collector0740c18 dokoncil nad frozen runtimef572599
vsech30+30 startu,3 paired serie po301 idle samples a stejnych200 history
entries. Nezavisle bylo overeno vsech1806 sample values, presne casy/PID/
scope/image i cely C#managed/NAudio/icon bundle a identicky corpus.

| Full-runtime signed-out profil | C# | Rust |
| --- | --- | --- |
| Process -> readable ready, p95 | 2543.1742ms | 496.9050ms |
| Mean private, MiB | 94.846 | 3.484 |
| Peak private, MiB | 107.148 | 4.297 |

Paired mean private ratiosRust/C# jsou3.8734%,3.4789%,3.6725%; kazda serie
splnila puvodni50% cil a80MiB maximum. Startup splnil<=1s i<=70%baseline.
Artifact client-comparison-readable-0740c18/full-client-signed-out-comparison.json
SHA2567668994d5c818e086d49d6d6acace3716d48db883c07cb67292f16801cddd886.
Jde o sdileny uzivatelem schvaleny stroj, nikoli exkluzivni idle OS nebo cold
cache. Native client sam v tomto profilu microphone/clipboard/network nepouzival;
samostatne schvalene kratke physical count-only probes probehly na hostu az
po dokonceni startup trials. Vsechny raw vzorky vcetne techto host podminek
zustaly zachovane, zadna serie/outlier se nevyloucila. Full migration accepted
status je stale false a signed-in/cloud/full-controller-hour nebyl domyslen.

## Jediny povoleny manual clipboard UI workflow

Po samostatnem souhlasu uzivatele byla spustena explicit-root/IPC izolovana
Rust aplikace bez OAuth, dictation shortcuts nebo autostartu. Owned process
image/argumenty byly overene; zadna osobni cache/history/token se nepouzila.
Computer Use provedl jednu manual Copy latest akci nad presnym synthetic
markerem. Pred paste se overoval POUZE clipboard-owner PID/sequence, ne obsah:
owner byl vlastniPID30728/sequence197. Stary clipboard se necetl/zaloha/
restore neprovadely a dalsi clipboard overwrite se nezadal.

Dva keyboard-dispatch pokusy server odmitl jako no_viable_candidate, aktualni
pole bylo nadale prazdne; shell/SendInput/WM_PASTE workaround se nepouzil.
Uzivatel pak na konkretni handoff provedl jednoCtrl+V do vlastniho neulozeneho
Settings Backend URL pole. Agent read-only accessibility tree overil Value
presne "VoicePrompt synthetic clipboard test 2026" a Settings zavrel bez Save.
Persistovana synthetic settings/history zustaly byte/hash-exact a token cache
nevznikla. Test nic neposilal, nepouzil Enter, microphone/hotkeys ani cloud.

Vysledek je passed_with_manual_paste_assistance. Prokazuje native History/
manual-copy/clipboard a skutecny standard paste do vlastniho UI, nikoli
automaticky dictation paste/focus-guard nebo podporu bezpecneho foreground
dispatch na tomto hostu. Scope/souhlas je spotrebovany; dalsi copy/paste se
neprovadi a podle schvaleneho dusledku zustava marker v clipboardu bez restore.
Own fixture ma600s bounded lifetime; po skryti okna se overi jeho skutecny exit.

Owned UI process30728 nasledne skutecne skoncil na svem bounded limitu;
matching frozen executable uz nebezi a agent nepouzil force-kill ani neukoncil
osobni aplikaci. Explicitni synthetic input/result soubory zustaly v session
artifacts jako evidence, nikoli user data/autostart/installed konfigurace.

Code checkpoint cc3367b ma successful vsechny Rust Windows/Linux, obeMSRV1.85
a non-root container jobs i pure physical-telemetry guard:
[rust-ci41417561087](https://github.com/tkubica12/cot-voice-recorder/actions/runs/41417561087).
First-PCM instrumentation07ac31e ma navic successful standalone Windows CI.
Aktualni runtime se proti poslednimu frozen client/engine mereni menil jen
v explicitne ignored manual telemetry probe; normalni installed/client logika
se temi physical testy ani collector zmenami nemenila.

## Zbyvajici real-world hranice po techto dukazech

Implementace Windows/cloud je source-only propojena a povolene bounded
synthetic/reference/physical checks uvedene vyse jsou skutecne provedene.
To stale neni vyrok "vse perfektne otestovano" nebo produkcni cutover.
Zbyva live Google sign-in/out a signed-in client traffic, full GUI/controller
hodinovy lifecycle, actual automatic hold/toggle dictation focus-guard/paste
chovani a screen-reader/multi-DPI/keyboard akceptace. Dosavadni manual paste
potreboval lidskou asistenci, proto nezastira safe keyboard-dispatch limit hostu.
Actual per-user installer upgrade/rollback ani real private Azure/UAMI runtime/
distributed fault/cutover nebyly povolene/provedene; zadna resource/access/
topology/deploy nebo placena AI zmena se neudelala.

Tento pocitac je uzivatelem schvaleny a dalsi profil se nevyzaduje. Dalsi
fyzicke/account/install/cloud akce ovsem nelze pripojit ke spotrebovanym
jednorazovym mikrofon/clipboard souhlasum. Pokracovani v techto branach
vyzaduje konkretni samostatny scenar a odpovidajici opravneni/prostredi,
nikoli dalsi odhad nebo preznaceni partial evidence jako kompletniho PASS.

Dve dalsi non-physical native DPI regressions pouzivaji pouze vlastni hidden
STATIC/EDIT/BUTTON controls: exact96->144->192->96 layout roundtrip, actual
system-message font heights a WM_GETFONT bindings, suggested bounds bez
zobrazeni/aktivace. Nemeni OS scale, nepresouvaji viditelne user windows a
nepouzivaji keyboard,clipboard nebo zarizeni.235 regularnich Windows Rust
testu a strict fmt/Clippy proslo; tri vyrazne oznacene long/physical fixtures
zustaly ignored. To je konkretni automaticky DPI dukaz, ne tvrzeni o realnem
screen-readeru nebo vsech monitorech/keyboard cestach.

## Ephemeral HTTPS Azurite adapter contract

Zbyvajici cloud-side kontrakt se rozsiruje o skutecky emulovany Blob/Queue/
Table service, ne druhou implementaci REST odpovedi v nasem mocku. Ignored
Rust test pouziva real AzureStorage a ManagedIdentity HTTP adapter s explicitni
loopback-only identity fixture a HTTPS endpoints. Public self-signed fixture
certificate je trusted pouze v konkretnim test clientu; zadne globalni trust/
security zmeny, invalid-cert acceptance nebo plaintext Storage endpoint.

Existujici oficialni Azurite image3.35.0 byl overen z MCR a je pinned na
amd64sha256:dae2a5f96553962901304b94e72ef87e299d0825e4b679673bcc527a25076fe4.
Fixture nepouziva loose mode ani skipApiVersionCheck. Certifikaty/tokeny jsou
ephemeral synthetic, Docker porty publikovane pouze127.0.0.1. Admin setup
pouziva existujici verejny Azurite dev account z reference; Rust data-plane
requests nesou synthetic token pres actual injected MI adapter.
Azurite OAuth basic overuje issuer/audience/expiry, NE signature/permissions;
test tedy nesmi tvrdit real UAMI/IAM nebo private Azure akceptaci.
Viz [oficialni OAuth/HTTPS pravidla emulaturu](https://learn.microsoft.com/azure/storage/common/storage-install-azurite#oauth-configuration).

Scenar overuje read-only resource probes, actual Table CAS/ownETag a typed
Int64 s aktualnim Azure SDK v obou smerech, Blob create-if-absent/lease
renew/release/takeover/stale-owner fencing a Queue receipt rotation,
visibility/redelivery i UTF8/base64 SDK interop. Produkcni auth/storage
konfigurace ani behavior se zatim nemeni, neni pridan shared-key fallback.

Trio local certificate/identity/endpoint guards, strict Rust Clippy/fmt a
osm existing native Storage tests prosly. Lokalni Docker daemon zustava
nedostupny; actual emulator run je teprve zapojeny do existing backend-ci,
kde uz existuje Docker a approved locked Python graph. Success report se
publikuje az po stopped owned container/identity a smazani ephemeral private
key. Artifact allowlist nikdy neuploaduje key nebo cely temporary root.
Do prvniho skutecneho successful CI behu tento emulator contract neni PASS.

Prvni actual run18a8bc1 prosel HTTPS resource probes a Rust bearer Table read
SDK-created Int64, ale selhal na prilis prisnem testovacim predpokladu:
GET s odata=nometadata skutecne vratil Int64 string, ne jeho type annotation.
Type check se proto presunul na explicitni fullmetadata query a actual Azure
SDK EntityProperty.edm_type/value; Int64 semantika se nezmekcuje na obycejny
string. Produkcni adapter/endpoint/auth behavior se touto opravou nemeni.
Failure artifact/log je zachovany a neni vydavan za completed contract.

Ctvrty local guard nyni provadi actual loopback HTTPS handshake: client
s presnym fixture certificate uspel a default untrusted client selhal
s CERTIFICATE_VERIFY_FAILED. Nejen parsovani certificate tak overuje, ze
se TLS verification nevypina. Vsechny ctyri local guards a native storage
compile/tests prosly; actual emulator rerun nasledne rozhodne zbytek kontraktu.

Druhy fullmetadata check33e739b pak zachytil skutecny adapter bug:
native row po GET->CAS replace uz nemel Edm.Int64 typ. Nometadata read
zahodil property annotations a nasledujici raw-entity replace serializoval
puvodni typed hodnotu jako Edm.String. AzureStorage.entity nyni vyzaduje
fullmetadata a zachovava Int64/DateTime annotations. HTTPheaderETag zustava
jedinou autoritou pro CAS; bodyodata.etag se odstrani a nesmi prepsat verzi.
Nepouziva se wildcard, retry-overwrite ani typova heuristika.

Prime socket regression overuje requested metadata level, Int64+DateTime
roundtrip, retained property types a staleCAS412. Osm native storage testu
a strict Clippy/fmt proslo; stejny actual SDK/emulator scenar se musi znovu
provest bez zmeny ocekavane stored-type semantiky. Jde o real opravu zachovani
cloud dat, nikoli dalsi povoleni k Azure deploy nebo sahani na osobni stav.

Actual016a5ac rerun uz prosel celym primitive contractem a actual SDK interop:
Table Int64 zustal typed po CAS update, staleETag PUT/DELETE vratily412,
Blob stale owner nemohl obnovit/uvolnit/smazat cizi lease a Queue stary
receipt neodstranil winner message; redelivery zachovalo payload a zvysilo
dequeue count. TLS a APIversion checks zustaly zapnute, vsechny tri sluzby
sdilely presne jeden cached synthetic MI token request. Owned container/
identity skoncily a ephemeral privatekey byl odstranen pred success report.
[backend-ci41426538580](https://github.com/tkubica12/cot-voice-recorder/actions/runs/41426538580)
i cely016a5ac Rust Windows/Linux/MSRV/container CI jsou successful.
Artifact azure-rest-016a5ac/azure-rest-contract.json SHA256
e9cbeda2afdc6c1b658523bbc1696b04fec609b16dbb186c7ba7b4a6a6a91d30.
Emulator OAuth signature/permissions a skutecny Azure UAMI/private egress
zustavaji vyslovne NEPROKAZANE.

Navazujici bezpecny ignored scenar nyni sklada actual Azure-backed Services,
QueueWorker, legacy processor/finalizer a cleanup nad stejnym HTTPS Azurite.
Model a PubSub jsou vyhradne existing bounded loopback providers s presne
synthetic WAV/textem, ne placena AI ani live notifikace. Scenar vyzaduje
completed+durable delivery state, exact provider audio, settled replay bez
dalsiho model call, actual SDK retrieval transcriptu/fixed48h dates a expired
cleanup, ktery pres aktivni Blob lease metadata/body zachova a po release
oba odstrani. Typed proof guard odmitne stale oracle nebo chybejici casti.

Tato slozena E2E cast se teprve provede v CI; devet existing worker tests,
compile/Clippy/fmt a pet local TLS/MI/typed-proof guards prosly. Nerovna se
dosud provedene primitive storage proof automaticky proofu celeho workeru.

Slozeny5ed21b6 run nasledne prokazal dalsi actual wire bug: ASR/chunk
durable save prosel, ale finalizer zustal Transcribing, protoze create
prazdneho canonical lock Blob vratilo400. Vec::new request nemel explicitni
Content-Length:0, ktery Put Blob vyzaduje. Byte-level local socket test
stejnou chybu reprodukoval nezavisle na emulaturu.

Azure request writer nyni nastavuje presnou byte Content-Length pro kazde
Some(body), vcetne nuly; nevklada se chunked transfer, nove retry ani
success-shaped fallback. Prime regression po oprave prosel a actual
emulator scenar ma navic explicitni empty-lock create/get/delete roundtrip
pred workerem. Devet native storage tests a strict Clippy/fmt proslo.
Reference je [Put Blob required headers](https://learn.microsoft.com/rest/api/storageservices/put-blob).
Nove actual worker/cleanup E2E se musi dokoncit v CI, prvni failed run
zustava zachovany a neni vydavan za completed proof.

5060751 actual worker/cleanup rerun ted prosel:
[backend-ci41430865698](https://github.com/tkubica12/cot-voice-recorder/actions/runs/41430865698).
Azure SDK precetl skutecne completed recording/delivery flag, transcript
body a typed dates s presnym48h rozdilem. Native scenar overil presne
model WAV bytes, settled replay bez dalsiho model request a active-Blob-lease
cleanup fencing v realnem emulator service. Empty-lock create/get/delete
tak neprosel jen HTTP mockem, ale i pres HTTPS Azurite. Report
azure-worker-5060751/azure-rest-contract.json SHA256
5dac41fecf0cd3dc4745b1a8b40e756e2612863ef03535fb1bb4af8b6e7799b1
byl nezavisle overen spolu s typed native proof, server status logs a absenci
privatekey/failure artifacts. Model/PubSub zustaly synthetic loopback;
real Azure/IAM/private endpoints a paid AI to neprokazuje.

Nezavisly5ed21b6 Windows parity job jednou prekrocil20s pri uplne prvnim
cold C#reference unprotect(empty); pozdejsi bidirectional DPAPI cases prosly.
Exact OS/CLR pricina bez startup trace neni prokazana. Funkcni priprava
reference je nyni samostatny bounded60s protect nad znamymi synthetic bytes
s elapsed logem; dalsi crypto operations stale maji20s. Zadny runtime,
parity expectation nebo mereny startup/performance threshold se nemenil.
Sest actual DPAPI/IPC/startup parity groups proslo lokalne, bez osobnich
token/history/recovery nebo dalsiho microphone/clipboard testu.

Navazujici ignored actual-storage continuous scenar pouziva dva durable
contiguous segmenty160000+320 samples. Real QueueWorker/recording lease/
stream processor komunikuje jednim existing MAI transportem s lokalnim
synthetic WS providerem, nikoli realnym modelem. Vyzaduje presne320640
PCM bytes bez reset/silence/duplication, pouze jeden final commit a persisted
160320-sample cursor. Source audio musi zustat i po ASR-ready az do durable
canonical finalization; teprve potom musi byt obe Blob audio casti gone.
SDK overi completed/delivery metadata, actual transcript body a48h dates.

Jsou to stejna actual HTTPS Azure ports jako v legacy/cleanup proofu,
tokeny/storage/model/notifikace zustavaji explicitne synthetic/loopback.
Compile/Clippy/fmt, ctyri existing ordered continuous regressions a pet
local safety/proof guards prosly. Tento novy composed continuous scenario
se musi jeste skutecne provest v CI a nelze jej odvodit z legacy PASS.

Prvni1c92826 continuous run dosahl ASR-ready/160320 samples, ale test pak
chybne hledal legacy id/index.wav. Actual streaming storage spravne pouziva
persisted content-addressed blob_path s digestem. Test nyni zmrazi obe
skutecne Chunk metadata paths pred modelem, pred finalizaci overi byte-exact
WAV v obou objektech a po ni vyzaduje NotFound na techze paths. Stored data
layout se nemeni ani neprevadi na legacy naming; opraven byl jen testovaci
predpoklad. Ctyri existing continuous regressions a strict Clippy/fmt prosly;
cely composed scenario stale ceka na successful actual rerun.

12b825a actual HTTPS storage continuous rerun prosel v
[backend-ci41433417086](https://github.com/tkubica12/cot-voice-recorder/actions/runs/41433417086).
Jeden leased model stream zpracoval exact160320 samples ze dvou ulozenych
contiguous segmentu s jedinym final commit; audio bylo overene byte-exact
po ASR-ready a gone az po durable finalization/delivery. Azure SDK overil
persisted160320 cursor, completed+delivery metadata, actual transcript body
a48h dates. Report azure-continuous-12b825a/azure-rest-contract.json SHA256
316f9c0e21f1d8215460f91581fdedd794eb49568f493b2aa95d4047b4fcd0a6.
Tls/version checks, loopback-only fake model/PubSub a real-IAM=false limity
zustaly stejne; owned emulator/identity/privatekey cleanup je potvrzeny.

Posledni navazujici fault case vyzaduje take actual-storage replay po
synthetic model error hned po prvnim packetu. Pred novym pokusem musi byt
ASR-ready=false, zadny raw-result pointer, uvolneny puvodni recording lease
a obe exact audio Blob casti zachovane. Novy leased stream musi znovu
odeslat kompletni audio od nuly, ne partial cursor nebo novy model per
segment; az druhy pokus smi potvrdit final text. Explicitni duplicitni wake
nahrazuje pouze cekani testu na retry delay, nemeni produkcni backoff.
SDK vyzaduje stream_attempt=2; zadny timer, retry limit ani safety invariant
se nesnizil. Compile/Clippy/fmt,4 existing continuous regressions a5local
typed-proof/TLS/MI guards prosly; actual fault run jeste ceka na CI.

8b638d5 replay-after-model-error case nasledne prosel v
[backend-ci41434473034](https://github.com/tkubica12/cot-voice-recorder/actions/runs/41434473034).
Actual HTTPS storage potvrdil failed attempt1 bez ASR-ready/raw pointeru,
released ownership a retained exact audio; attempt2 poslal full160320 samples
od nuly a pouze jeho final commit vedl k durable transcript/delivery.
Azure SDK navic overil attempt2 a stored cursor/body/dates. Report
azure-replay-8b638d5/azure-rest-contract.json SHA256
27a41cdd4cf1aaac113f2a116d678ebebf792e165faadd75211be8dad41dd070.
Prvni healthy12b825a Windows/Linux/MSRV/container RustCI41433417304 take
prosel; cold-reference preparation dda696e RustCI41431754015 prosel.

Uzivatel pak samostatne schvalil jeden live Google sign-in/out scenar
v nove izolovane Rust app/DPAPIcache bez produkcniho backendu/realtime,
osobni cache nebo paidAI a prevzal browser account/consent krok.
Povolil minimalni cilenou discovery existujiciho desktop OAuth configu;
repo-staged JSON byl nalezen a jeho client ID se shoduje s presnou znamou
instalovanou konfiguraci, bez vypsani hodnot nebo zmeny souboru.
Zadny osobni token/history/recovery nebyl cten.

Pri prime kontrole source se opravil predchozi konzervativni, ale nespravny
predpoklad o Google revocation: Rust AuthRuntime::sign_out i referencni
C# AuthManager.SignOut pouze mazou lokalni cache a vypnou memory bearer.
Google revoke endpoint se nevola. Remote revocation se do kompatibilniho
chovani kvuli testu nepridava a nebude se tvrdit.

Novy explicitni auth-only fixture ma own marker/root/IPC, bounded lifetime,
exact official Google endpoints a nejvyse jeden sign-in/out attempt.
Odbocuje pred vytvorenim ApiClient/RealtimeClient/coordinator/outbox/devices;
fyzicke porty a ostatni UI commands jsou blokovane. Vysledek obsahuje
pouze Boolean DPAPI-readback/local-signout/stale-bearer stavy a PID.
Opakovane pouziti stejneho rootu je odmitnute. Jde o pripravu schvaleneho
live scenare, nikoliv zatim dukaz provedeneho Google loginu.

Priprava prosla241 regular Windows workspace Rust testy, fmt/Clippy
all-targets a release buildem. Pet novych nonphysical guards overuje bounded
parser, official endpoints, exclusive safe Boolean report, structurally
backend/device-free composition/rejected commands a joined local sign-out
shutdown nad pouze syntetickou DPAPIcache. Manual microphone/engine-hour/
Azurite cases zustaly v obycejnem behu ignored; zadny live login se v
unit testech nespustil.

Jeden samostatne schvaleny b27bba6 live Google scenar potom skutecne PASS:
user osobne dokoncil browser account/consent a potvrdil "Prihlaseno".
Own native PID10868/root+IPC/window byly nezavisle overene proti frozen
binary SHA256b233961943ed7453ae65c6bc692a7a24817cb9acb2537d0312726fea659a96d9;
UI preslo SignedOut -> SignedIn -> SignedOut. Google callback/token exchange,
actual DPAPI readback ID+refresh tokenu a dostupny ID bearer prosly; po
jedinem UI Sign out byla owncache odstranena a bearer vendovani vratilo
None. Backend/realtime/device/outbox composition nevznikla a Google revoke
request nebyl poslan. Osobni cache/history/recovery nebyly cteny ani meneny.

App a quit helper skoncily vlastnim normalnim izolovanym IPC, reportclosed
je true, own settings zustaly byte-exact a zadny own tokens/history/recovery
soubor nezustal. Exit code UI-shell-launched procesu nebyl pozorovan a
neodhaduje se. Native safe report google-auth-b27bba6/state/auth-fixture-result.json
SHA256f733e694c0101e02e23d6e97bbb90a72f68783b44a2b1ebe9b23edae989456fc.
Opravneni je SPOTREBOVANE, bez implicitniho dalsiho live Google pokusu.
8b638d5 full RustCI41434473213 take nasledne PASS; b27bba6
RustCI41436867879 jeste bezi. Tento PASS nezastupuje production backend,
remote revocation, actual refresh lifecycle ani plnou physical dictation/
installation/cloud akceptaci; whole_migration_acceptance zustava false.

Navazujici signed-out/physical-disabled native Settings kontrola overila
actual accessible role/name/value pro5 edit fields a5 checkboxes,
otevreni/Cancel bez ulozeni a unchanged synthetic settings. Jeden posted
Tab backend oznacil done, ale AX tree neukazal pozorovatelnou zmenu focusu;
proto se to NEOZNACUJE za keyboard navigation ani screen-reader PASS.
Owned PID35220 skoncil pri180s boundu, zadny token nevznikl a clipboard/
microphone/hotkey/paste akce nebyly provedeny. Duvod chybejici focus
telemetrie se nezamenuje za prokazany application bug.

Live flow soucasne odhalil privacy nedostatek prezentace callback odpovedi:
text/plain stranka dovolovala browseru pouzit callback URL jako window
title. Nasledna oprava vraci pouze staticke HTML s neutralnim titulkem,
no-referrer, strict no-resource/frame CSP a nosniff; no-store zustava.
Query material se nevklada do HTML a message/status/state/PKCE/deadline
semantika zustava. Actual TCP valid/wrong-state regression nejprve RED,
pak GREEN s exact byte length/title/headers/no-query-reflection.
Novy real Google login se kvuli oprave neopakoval; zmena nema tvrdit
odstraneni callback query z adresniho radku/browser history.

Client protocol suites71 cases a workspace Clippy/fmt prosly. Aktualni
per-user installer byl pouze znovu SESTAVEN (ne spusten/instalovan),
SHA256aea9b71afbc76cc8271dc815945525f1e4f41baeee173465ccf218a811442d6e.
Obsah stale pouze explicitni native EXE+icon, zadny OAuth config/secrets.

b27bba6 full Windows/Linux/MSRV/container RustCI41436867879 nasledne PASS.
0b7d992 callback privacy RustCI41438922788 jeste bezi.

Nejmensi dalsi physical port case byl konkretne predan koordinatorovi k
NOVEMU souhlasu: jeden fixed-marker clipboard overwrite a jedine automatic
Ctrl+V pres skutecny DesktopGuard do vlastniho prazdneho test edit pole.
Uzivatel by osobne klikl Run once a1s nemenil fokus/klavesy; control
vlastniho PID/window/edit focusu, modifiers a clipboard sequence predchazi
copy/paste, pri selhani zadny retry. Stary clipboard se necte/nezalohuje/
neobnovuje, nebezi microphone/hotkeys/account/backend a neni Enter/Save.
Dosavadni Google, mic ani manual-paste souhlasy to NEPOKRYVAJI.

Zatim pouze compiled opt-in ignored test-only fixture, ne provedena akce.
Vyzaduje fresh explicit current-root marker a exclusive result pred
vytvorenim okna; opakovany root se odmitne, normalni installer jej
neobsahuje. Native49 regular +7 startup tests a workspaceClippy/fmt
prosly,4 native manual/long fixtures zustaly ignored. Novy ownership/
exclusive-result guard nepouziva real clipboard ani UI. Skutecny manual
test se do ziskani presneho souhlasu NESPOUSTI; neprokazuje hold/toggle/
whole-controller hour nebo celou physical akceptaci.

Uzivatel pak jeden takovy automatic guarded-paste case vyslovne schvalil.
fa27f86 frozen ignored fixture se spustil ve vlastnim rootu, PID45552/
window5246560 byly overene, zadny agent Run klik nebyl proveden.
Okno ale neocekavane skoncilo pred zaznamenanym attemptem:
original report attempted=false, clipboard_write_attempted=false,
input_sent=false a closed=false; process i console window jsou gone.
Report SHA2569c795d16e23e9768ab6ba0ccba61ff7594b57c1e5db1b7a5516750232c2d1ba5
je zachovany byte-exact. Neni to PASS ani prokazany guard rejection.
Exit code a puvodni stderr nejsou dostupne, presny duvod tedy neni
diagnostikovan. Koordinator dostal konkretni dotaz, zda uzivatel vubec
stihl Run kliknout; zadny automaticky novy start/retry se nekonal.

Prime nonphysical Windows file-lock regression dokazal, ze mutable
atomic report replacement muze selhat pri drzenem reader handle.
Nesmi se to vydavat za prokazanou pricinu puvodniho ukonceni.
Fixture proto nyni publikuje pouze immutable once-per-phase started/
armed/delivering/result/closed proof; pred clipboard API musi uspesne
publikovat delivering phase. Zadny report se neprepisuje pri kazde
UI zprave a reader nemusi uvolnovat replacement target. Sanitaized
failure se uchova exclusive v own rootu, vsechny stare case roots se
odmitaji. Pod aktivnim blokujicim synthetic readerem regression GREEN,
native50 regular+7 startup/fmt/Clippy PASS, physical case stale ignored.
Stara evidence nebyla prepsana a oprava se netestovala novym clipboard
overwritem nebo paste bez vyjasneni presneho opravneni.

Uzivatelovo jedine skutecne pozorovani bylo "Okno nevidim"; nepotvrdil
Run klik ani zavreni okna. Koordinator proto case vyslovne uzavrel jako
neprovedeny/bez prokazaneho attemptu a povolil pokracovat original-icon/
live-UI auditem a pouze synthetic fixture opravou. Nove otevreni
guarded-paste procesu potrebuje predchozi vyslovne rozhodnuti; neni
automaticky povolene. Zadne dalsi domnenky o uzivatelove akci se nepridavaji.

### Original icon: resource a packaging inkrement

Po preruseni byl znovu overen skutecny refactor-rust/da1c6eb checkout a
zachovany ocekavane necommitnute icon zmeny; zadna nova physical akce
nebyla provedena. Actual puvodni Assets/app.ico je nyni build-time
RT_GROUP_ICON1 v native EXE, nikoli obecna systemova ikona v isolated
rezimu nebo pouze sidecar v installed rezimu. Small/big window ikony,
tray a Settings pouzivaji stejny embedded original asset; window/Settings
reaguji na DPI. Installer setup, uninstall i start-menu/desktop shortcuts
maji original ICO. Shared resource handles se nesmi DestroyIcon.

Zvoleny winresource=0.1.30 byl pred pridanim overen v povolenem crates.io
sparse feedu a resolved pod deklarovanym MSRV1.85 bez upgradu existujicich
locked zavislosti. Resource compiler je jiz existujici Windows SDK, zadny
novy tooling nebyl instalovan. Actual compiler1.85 overeni nove graph
teprve doda nove CI; resolver neni nahrada compiler proof.

Workspace244 regular Rust tests, fmt a Clippy-Dwarnings PASS.
Novy native binding test overuje obe WM_GETICON velikosti pri96/120/
144/192/288DPI bez Show/focus zmeny. Sedm actual-release platform parity
groups PASS vcetne byte-exact porovnani VSECH original ICO entries a
RT_ICON payloads v actual release EXE, nejen existence resource.
Native EXE SHA25697ceed2cd1d03d094e6939b58f8a8f4104968a1f2e1f31b84f038ecd76543019.
Build-only installer SHA25678210c20887917ece6837e56098d80a548a9384769a9f098d7c299d264c5b40f,
nikdy spusten/instalovan. Original ICO zustal byte-exact
SHA2565c43009a56fd315d150a0cbf4574c7dda858296a0006321ba5647023f72faa42.

Tento resource/source PASS jeste NENI screenshot/visual acceptance.
Actual WPF DictationIndicator ma440x112DIP rounded17 dark(30,30,34)
panel,8px state dot,12px header,14px preview/20px lines, work-area
bottom-center18, noactivate/click-through a900ms saved fallback.
Dosavadni Rust plain STATIC border/bottom-right/bez clocku a s
neomezenym terminal popupem tomu neodpovida. Nasleduje skutecny native
renderer a reactive controller parity; stara performance evidence
se na tyto budouci zmeny neprepisuje.

da1c6eb Windows/Linux/MSRV/container RustCI41441386069 a predchozi
fa27f86 RustCI41439973457 nasledne PASS. Cela migrace zustava nehotova.

### Puvodni zivy dictation popup: actual native/WPF vizualni srovnani

Plain bordered STATIC byl nahrazen native layered/click-through cardem
pres Direct2D software/DirectWrite, bez WebView. Geometrie, rounded17,
dark(30,30,34),8px OrangeRed dot pri recording opacity0.5 a blue dot
pri finishing,12/14px Segoe UI,20px line spacing, placeholder a primary
work-area bottom-center18 vychazeji primo z original DictationIndicator.
Clock mm:ss a rostouci preview se aktualizuji; po genuine final completion
bez paste se jednou zobrazi "Saved - paste skipped" s FINAL confirmed
preview a900ms dismiss. Pending/recovery/error nejsou timto completed
stavem maskovany; zustava bezpecny tray warning a Recovery bez autopaste.
Escape latch, delivery cancellation/focus/clipboard guards zustaly zachovany.

Actual original C# DictationIndicator a actual Rust UI renderer byly
spusteny pouze v bounded visual-only fixturech s fresh marked roots,
bez AppHost/device/account/backend/clipboard/hotkeys/paste portu.
15 paired rasters pokryva empty/growing/wrapped-ellipsis/finishing/saved
stavy pri96/144/192DPI. Visual review odhalil1DIP preview baseline rozdil
a final-word wrapping pred ellipsis; obe odchylky opraveny proti actual
WPF rasteru, nikoli novym stylem. Posledni comparison report
overlay-visual-8cd3a26-c/comparison/comparison.json
SHA25651d3e946451026d51270e99e3f09c4d15e8b3a1b8b6180eac2c4e76b2cab1f52.
Obrazky jsou velmi podobne, ne pixel-identicke: zbyvaji drobne rozdily
font/edge antialiasingu a v96DPI muze last fitting character pred
ellipsis vyjit jinak mezi WPF a DirectWrite.

Prvni native preview fixture nepovolil process DPI awareness; jeho
virtualized96DPI placement se NEVYDAVA za product DPI proof. Opraveny
fixture nastavi stejny process-only per-monitor-v2 jako actual klient.
Nasledne5 native stavu maji actual144DPI bounds[870,1333,1530,1501],
stejne jako reference; kazdy Present zachoval foreground, terminal
se skryl po900ms a neobnovil se na nezmenenem statusu.
Vsechny own visual procesy byly overene ukoncene a own HWND zaniklo.
Ani tento proof nezastupuje real monitor-switch, keyboard/screen-reader
nebo automatic guarded-paste akceptaci. Stary paste case se NEOTEVREL.

Drawing surface/font resources jsou lazy a na hide se uvolni; nezmeneny
frame se znovu nerenderuje. Samostatny nonvisible graphics-only regression
po20 warmup a1000 render/hide cyklech: GDI handles10 ->10,17.56s.
Neni to hodinovy workload ani memory acceptance. Native source stale
neaktivuje okno a pri physical-disabled composition nema clipboard cestu.
Tray popup je original Open(default)/Copy latest/Pause notifications/Exit
s live realtime tooltipem, checked pause, empty-copy disabled a puvodni
double-click/keyboard/balloon aktivaci; Reconnect zustava v main Settings.

249 regular workspace Rust tests/fmt/Clippy/release,7 release platform
parity groups a3 pure raster export guard groups PASS. Native icon test
nove overuje actual bitmap rozmery pro obe velikosti pri96/120/144/192/
288DPI, nejen shodu WM_GETICON handle. C# visual-only reference build ma
0 warnings/errors. Installer opet pouze SESTAVEN, nikdy instalovan:
SHA256d6602eaf0877ae3afeb1b44aeadc83475974c47aa662baac61805a63e1050f3e.
8cd3a26 full RustCI41451827924 PASS vcetne actual MSRV1.85 noveho
winresource graph. Reactive-overlay CI a nove post-UI startup/idle
mereni teprve nasleduji; stara f572599 cisla se na novy build neprepisuji.

7b4aff4 reactive-overlay full RustCI41456349495 a original C#WindowsCI
41456349586 nasledne PASS. Novy frozen7b4aff4 signed-out30+30/3 paired
301-point comparison nyni bezi v oddelenych explicitnich synthetic roots;
native SHA25637f83a8302992a8acd67b500d5ac0d273fc638f31f3998fb723512b34135cd21.
Dokoncene dilci samplery ani zive own-PID readiness zatim nejsou
finalni performance acceptance a nejde o controller/physical hour.

### Bezpecna observer diagnostika bez znovuotevreni paste akce

Novy observer-only entrypoint ma JINY scope a event loop nez actual
guarded-paste case. Run je disabled, vlastni prazdne pole read-only,
ARM se odmitne pred clipboard/focus/native-input porty. Scope markery
jsou vzajemne vylucne a stare action/observer phase roots se odmitnou.
Phase soubory zustavaji immutable a closed se publikuje az po zaniklem
own HWND. Shared fixture shell ma original icon, DPI-aware fonty,
explicitni background a work-area centrovani/initial show bez aktivace.
Normalni installer tuto cfg(test) diagnostiku neobsahuje.

Prvni60s observer se sam normalne uzavrel bez akce; jeho titulkovy prefix
byl v CUI katalogu aliasovan na installed VoicePrompt, tedy se nevydava
za uspesny AX-control proof. To NENI dukaz priciny stareho fyzickeho
case exit. Testovaci titulek je nyni zamerne odlisny:
"Isolated Rust observer - no paste", maximalni bound3min.
V novem fresh observer rootu byly ownPID46448/HWND3014990/image
nezavisle overeny a CUI canonical process target skutecne precetl
disabled Run a read-only blank field;144DPI1050x495 panel a original
ikona byly take vizualne overeny. Jediny CUI klik byl Close, nikoli Run.
Po nem se HWND/process normalne zavrely, closed report ma veskere
action/clipboard/input flags false:
guarded-observer-7b4aff4-b/state/observer-closed.json
SHA256ca5a820fbaf413410252c661a553971c91bb904fcb75fc5c9bce596d5f599c74.
Own transient launchery byly odstraneny; raw dukazy zustaly.

Puvodni fyzicky case je stale uzavren jako NEPROVEDENY/bez prokazaneho
attemptu; nebyl otevren action-enabled proces ani novy clipboard/paste.
Koordinator dostal presny nejmensi novy scope pro vyslovne rozhodnuti
po signed-out mereni: nove3min okno "Isolated Rust test - guarded paste",
jeden osobni Run once,500ms guard delay, jeden fixed-marker overwrite
a nejvyse jedno DesktopGuard Ctrl+V do vlastniho pole, bez retry/Enter/
Save/jinych aplikaci/device/account/cloud portu. Stary clipboard se
necte/neobnovuje a marker zustane. Observer success toto opravneni
nenahrazuje a zadny drivejsi souhlas se nerecykluje.

### History input parity: primy audit puvodnich WPF handleru

Actual MainWindow.OnHistoryDoubleClick/OnHistoryKeyDown a F5 KeyBinding
odhalily chybejici native wiring, nikoli novy volitelny shortcut.
Native double-click/Enter/Space/Control-only C nad fokusovanym History
nyni pouzivaji stejnou manual cached copy cestu; Enter/Space zachovaji
puvodni WPF chovani i se Shift/Control. F5 je bez modifieru a respektuje
disabled Refresh. Own-window/child check predchazi vsem key-state reads;
Settings a cizi aplikace se neroutuji a nic se globalne neregistruje.
Event se spotrebuje pred IsDialogMessage, aby Enter misto copy nemohl
spustit jiny default button. Testy nevolaji input/clipboard API.

Copy original availability se nyni meni s aktualne vybranym radkem,
presne podle reference RawBody != null, ne podle nonempty History.
Pritom explicitne prazdny original zustava odlisny od null; nic se
nemeni na ulozenem JSON ani manual-copy/pause/auth privacy pravidlech.
Selection/keyboard tests, native59 regular+7 ignored/startup7 a cela
workspace252 regular/fmt/Clippy/release prosly. Neni to actual human
keyboard/screen-reader/clipboard acceptance. Prave bezici performance
comparison zustava frozen7b4aff4 post-overlay build; tyto budouci input
zmeny se do jeho source/binary identity nepodsouvaji.

### Post-icon/overlay signed-out performance: overeny frozen7b4aff4

Nova full-runtime signed-out serie skutecne dobehla bez restartu trialu:
30 alternating startu kazde implementace a3 paired301-point idle serie,
tedy60 startu/1806 raw samples se stejnym200-entry synthetic corpusem.
Puvodni cile NEZMENENY. Independent verification znovu spoctlo vsechny
p95/mean/max a kazdou paired serii z raw dat, overilo presny corpus,
vsechny11 C# bundle files/native binary SHA a6 own-PID sampler reportu.
Actual serie mely299.982..300.002s pri requested1s intervalu.

| Metrika | C# reference | Rust7b4aff4 | Scoped zaver |
| --- | --- | --- | --- |
| Startup p95 | 1340.6883ms | 455.1822ms | 33.9514% baseline, <=1s i <=70% PASS |
| Mean private | 73.9107MiB | 3.6501MiB | Kazda ze3 paired serii <=50% baseline PASS |
| Peak private | 85.2773MiB | 4.28125MiB | <=80MiB PASS |

Report client-comparison-overlay-7b4aff4/measurements/full-client-signed-out-comparison.json
SHA2562949d7070db3e26d836e1e435d5fdac264d9ac43d39c91600fb628d4a6954067.
Independent-verification.json uchovava recomputed summary, actual durations,
raw counts a vsechny process IDs. Collector exit0, vsechny measured own
procesy skoncily a vsechny generated state roots byly odstraneny;
frozen bundles a dukazy zustaly. Detach zde chranil bounded collector
pred drive pozorovanym host/tool interruption, nejde o dalsi agent session.

Toto je fresh regression original-icon/overlay buildu, nikoli odhad ani
prejmenovani stareho f572599 reportu. Neslibuje OS cold-cache start,
signed-in backend provoz, microphone/clipboard/paste, one-hour GUI capture,
autostart/installation nebo cloud latency/billing. Pozdejsi history input
commit ma vlastni binary identity; finalni aktualni full-feature profile
a zbyvajici physical/accessibility/upgrade/private-Azure akceptace tim
nejsou automaticky uzavrene. Cela migrace zustava nehotova.

### Viditelnost a SetFocus fixture: nove case dukazy, zadny tichy retry

Po novem vyslovnem souhlasu byl jednou otevren PID43660/HWND3739382.
User odpovedel presne "Okno nevidim". Case se normalne uzavrel v3min
boundu17:23:26 s closed=true a attempted/write/input=false.
Neni to PASS ani prokazany guard rejection. CUI AX-visible HWND neni
dukaz skutecne human visibility; predchozi tvrzeni "viditelne" bylo
prilis silne. Closed report SHA2565d73ab0f918bff8ea8ebba1e1f3e8b20ca9697eb2312efa560f2b4af6e65ac78.

Pouze observer-only prezentace pak dostala active-monitor placement a
temporary topmost se zachovanim foreground. Uzivatel ji SKUTECNE
potvrdil "Ano, diagnosticke okno vidim"; Run disabled/read-only pole,
jediny CUI Close, vsechny clipboard/input flags false. PID22452 skoncil,
closed SHA25601e81bc89933524e8aae15f702f5f920007c65250a36bd97a0b6ff78e5f7b5e9.
Toto potvrzeni samo NEPOVOLILO dalsi physical action.

Po dalsim samostatnem souhlasu vznikl jediny novy PID19340/HWND5836320,
frozen7e98e2f... se stejnou prezentaci. Asi3s po startu se proces ukoncil
s "The operation completed successfully. (0x00000000)" drive nez byla
dokoncena readiness/child Run otazka. Existuje started+failure, nikoli
armed/delivering/result/closed. Started=false je jen pocatecni snapshot:
NENI znamo, zda uzivatel klikl nebo zda byl Run event prijat. Proto
se nepise "nulovy attempt". Bez write-ahead delivering proof tento
build nemohl volat clipboard publication ani native paste. Failure
SHA256bf704f5c47e8acbf3ff102cb19e2eac7e1447b3763ab94f437927392c6492582.
Puvodni phase soubory zustaly immutable, zadny automaticky novy start.

Prime hidden-control Win32 repro dokazalo konkretni fixture bug:
DisableWindow na fokusovanem Run buttonu nastavi focus=NULL. Nasledny
SetFocus(edit) uspesne zmeni skutecny focus, ale vraci PREDCHOZI NULL,
coz binding projectuje jako Err(S_OK). Oprava je specificka pro SetFocus:
existujici own current-UI-thread target a overeny actual GetFocus;
nejde o obecne polykani Err(S_OK). Source physical fixture nyni
publikuje attempted OKAMZITE po prijeti Run a samostatne focus
preflight/focused s operation context, pred kazdym clipboard portem.

Samostatny explicit marked-root NoPorts proces provedl STEJNOU
post-Run posloupnost a6 immutable operation-context phases:
focus-attempted -> disabled(NULL) -> set(actual edit,projectionHRESULT0)
-> target-verified -> normalclosed. PID43588, exit0,60.586ms;
own GUIThreadInfo/PID/thread/parent/focus podminky vsechny true,
foreground unchanged, zadne okno nebylo shown, zadny clipboard/input
port ani DesktopGuard nebyl konstruovan. Tento jednotlivy hidden run
mel foreground unchanged, ale sam jeste neprokazal dostatecnou izolaci
pro vsechny podminky; nasledujici regrese ji zesilila. Focus-set proof
SHA256dcfe0d282858d53ac56a34bb7842988e381399d02d82cc65d268eb41d2bf33e4,
target proof47a761121ba03ebe59344d7a2ce4c0c06bfe2884931891bea80e6e366e963e62,
closed072693cc04c0fb86ba7bf127cf298d60270cd5c54192bfd42bc2f96de6549fc0.

Toto prokazuje interop contract a fixture opravu, ne actual foreground/
clipboard/SendInput guard ani zpetne presny stack chyby PID19340.
Global foreground guard nebyl v NoPorts predstiran. Pred dalsim live
oknem se musi dokoncit diagnostika a prijit dalsi vyslovne rozhodnuti;
zadny drivejsi souhlas se nerecykluje.

Rozsirena paralelni regrese nasledne zachytila4 foreground snapshot
failures, takze hidden-only boundary se NEVYDAVA za spolehlive overenou.
SetFocus muze podle Win32 aktivovat parent; presny owner drivejsiho
foreground HWND nebyl zaznamenan, tedy se jeho pricina nedotvari.
NoPorts parent nyni pouziva HWND_MESSAGE + WS_EX_NOACTIVATE, nema
desktopovy Z-order ani visible surface. Nejprve cilene API repro,
pak vsech62 native regular tests a cela aktualni workspace+Clippy
znovu PASS bez potlaceni predchozich failures.

Finalni separate NoPorts process proof s timto message-only parentem:
PID51592, normalexit0,32.159ms;6 immutable phases se specific operation
context. Disable->NULL, SetFocus projectedHRESULT0 a actual target focus,
GetGUIThreadInfo/PID/thread/parent/focus true, foreground unchanged,
window shown=false, zadne clipboard/input porty nebo DesktopGuard.
Focus-set SHA256cbd027fda3318205d0bbfeb560be4ccacec823d515f75762a3a54e1a6d89c90b,
target157f45e2c1aec4cb137b5ebcf66f1485d780cee4f553ff4d0af3375276cf625b,
closed9fcd1c5daaa16a9124c458c1947a36ae8d76e1eb2fe2522fbbba4eefa5310ca3.
Stejny Cargo-JSON executable discovery/explicit-root command byl
overen lokalne a je pridan do Windows CI s allowlist uploadem pouze
focus-*.json, nikoli libovolneho rootu.

V physical fixture je nyni separate ready/attempted/focus-preflight/
focused/armed/delivering/result/closed s operation context; started
se nikdy nezamenuje za konecny stav. Trvala aktualni omezeni:
NoPorts neprokazuje real foreground guard/clipboard/SendInput a
nevytvari dalsi live consent. Zadny live retry po techto opravach
nebyl proveden; dalsi uzivatelsky krok vyzaduje vyslovne rozhodnuti.

### Opraveny focus v real case, ale automatic paste stale NEPROKAZAN

Po dalsim NOVEM souhlasu byl jednou otevren fixed PID45788/HWND2166528,
frozen564b26bfa99a1a1d5e3e4a7fa18b6cbfacb875c0952eb155d02d7c3d97156a1b.
Drive nez agent dokoncil readiness, existoval jeden accepted Run,
focus-preflight/focused/delivering/result. Agent proto NEZADAL dalsi
klik. Nullable Focus oprava skutecne dosahla own target focus pri
projectionHRESULT0; target_matched=true a marker se jednou publikoval.
DesktopGuard vsak vratil false a fixture own observation nebyla
provedena. CUI sice videlo presny marker ve vlastnim poli, ale uzivatel
na jedinou upresnujici otazku vyslovne odpovedel:
"Klik na Run once a potom rucni Ctrl+V".

Vysledek je pouze assisted/partial, NIKOLI automatic-paste PASS.
Legacy Boolean nerozlisuje guard rejection od incomplete SendInput,
takze false se nepreklada na "zadne native input API nebylo volano".
Own field po rucnim paste tento rozdil nerozhodne. Case se normalne
uzavrel, process skoncilo a launcher se odstranil; dalsi retry neprobehlo.
Result SHA256ee02c3ff60e15b49289ce40c4a1f32e6b74b77246069971373c0dd114865fb59,
closed6fb62b815be8d3f813297064916c41563dd66deb666f57d63b53152de5475178.
Original clipboard se nikdy necetl/neobnovoval; fixni marker zustal.

Nasledna diagnostika zachovava VSECHNY puvodni guardy a jejich poradi:
sticky target/focus -> released modifiers -> publication sequence ->
unchanged sequence -> jediny4-event Ctrl+V. Typed outcomes ted rozlisi
presnou guard reason (vcetne expected/actual sequence METADATA, ne obsahu),
native emission invoked/events/expected a pripadny release-only cleanup.
Wrapper pro normalniho klienta nadale vraci true jen pro plny Ctrl+V batch.
Fixture navic zaznamena, zda own-field observation vubec probehla, a
viditelne zakazuje manualni Ctrl+V i po blocked vysledku.

Prime NoPorts unit cases pouzivaji STEJNY production decision/emission
algorithm s fake snapshots/callbacks: vsechny4 guard reasons nikdy
nedosahnou ani synthetic emitteru, order je lazy a focus latch sticky;
accepted counts0/1/2/3/4 i partial release se rozlisuji a cleanup muze
poslat jen V-up/Control-up, nikdy dalsi V-down/Enter nebo nekonecny retry.
Zadne nativni clipboard/sensor/SendInput volani v techto testech neni.
Dosavadni case evidence se temito novymi reason fields zpetne nedoplnuje.

### Responsive main UI: functional/reference-layout inkrement

Actual WPF720x540/min560x420 a History/Recovery sections nahrazuji
native570x730 all-controls canvas bez minimum/viewport handling.
Nativni taby oddeluji dve sekce, action rows se zalomi v min sirce a
History ma original list-oriented default; explicitni Show selected text
zachovava drivejsi read-only body view, neodstranuje ji z klienta.
Recovery ma original vysvetleni48h/256MiB/no-autopaste a explicitni
manual read-only Refresh (zaznamy se tim nemazou ani neslouci).
Stored formats, auth, pause/manual copy a dictation/recovery invariants
se nemeni. Clipboard controls zustanou disabled pri NoPorts fixture.

Nova physical-disabled actual full-client preview vytvori vsechny
skutecne komponenty nad vlastnimi synthetic settings/history/IPC,
bez credential discovery/ports. Default i minimum jsou bounded180s,
normalne se uzaviraji a kontroluji byte-exact unchanged files/no-token.
Pure geometry testy pokryvaji4 viewporty pri96/120/144/192/288DPI,
vsechny rects uvnitr a zalomeni footers bez overlap.

Actual CUI zjistilo, ze accessible tab selection muze zmenit TC state
bez TCN_SELCHANGE. UI ted reaguje i na zmenenou own native selection/
checkbox state v existujicim bounded UI ticku, nezavisle na status
channel changes. Prime CUI Replay nad FRESH compiled graph pak
overilo History -> Recovery -> History i read-only text toggle.
Drive pouzity cached native test binary bez teto opravy se nevydava
za corrected-source dukaz; nova fixture executable se zjistuje z
Cargo compiler-artifact, ne odhadem nejcerstvejsiho filename.

Default actual144DPI client1058x754 a reference-minimum818x574
(outer1080x810 a840x630) maji vsechny akce ve viewportu; u minHistory
je Clear History na druhem radku, Recovery help/list/3 akce se vejdou.
Byl proveden pouze indexed section/toggle navigation a read-only
Recovery Refresh, nikoli copy/paste/keyboard/Google/device/save.
Own preview data a dukazy jsou v native-main-layout-a75013b-current
a native-main-minimum-a75013b; UI/ready/closed source je odvozen ze
skutecneho klienta, nejde o staticky screenshot mock.

Tento inkrement je funkcni/layout/accessibility-state proof, NE plna
vizualni shoda: classic native buttons/system fonts, raw timestamp row
a multi-line status stale potrebuji puvodni WPF header/logo/palette/
font/row styling a actual reference srovnani. Zadna predchozi performance
cisla se na tyto dalsi zmeny neprepisuji a cela UI/migrace neni uzavrena.

### Actual WPF-like main theme: branding, live sections a repaint

Navazujici native main prenasi skutecny Theme.xaml/MainWindow.xaml:
40DIP embedded original logo, VoicePrompt header, SegoeUI13/11/17,
puvodni bile/sede/oranzove barvy, rounded4 status strip se tremi live
hodnotami, flat buttons a selected-tab underline. History ma original
dvouradkovy preview/localized local-date metadata,48h remaining a AI
suffix; original formats a copy guardy se nemeni. Actual WPF preview
navic odhalilo dedeni SemiBold z vybraneho TabItem do obsahu, ktere
se preneslo i do native History a section actions. Settings zustava
samostatne; zadny WebView/framework/runtime navic nebyl pridan.

Reference fixture ted otevre skutecny original MainWindow neaktivujicim
Show s explicitnim signed-out synthetic rootem; normalni ShowWindow
aktivace neni zmenena. Samotny tool musi linkovat original PNG do sveho
entry resource assembly a ICO do EXE. Drivejsi screenshot bez logo
resource se zachovava jako fixture chyba, ne jako puvodni design.
Fresh reference PID49292/144DPI720x540DIP ma oba original assets,
actual History/Recovery transition a normal closed/data-unchanged/noToken
proof45e026eeb57c5de62392761ae9b184c7aba96cd3cf1073de082e231f1413156a.

Actual native nahled zachytil tri coupled presentation chyby pred
akceptaci:34DIP tab item orezaval underline ve32DIP stripu; strip
prekryval Show selected text hit area; po section switch zustavaly
stare border/button geometry. Tab item je ted30DIP, oba sibling rects
jsou oddelene i v min sirce a parent po zmene geometrie prekresli
canvas s WS_CLIPCHILDREN. Actual fresh default/minimum corrected binary
5e425b2d77b979dec95732053625cb117fc08597098e4aab1330b71dc6759060
zobrazuje ciste History -> Recovery -> History a read-only text toggle,
bez starych car nebo prekryti. Default144DPI client1058x754 a min818x574
zachovavaji original outer720x540/min560x420DIP; min footer se zalomi
a lista je scrollovatelna. Dukazy jsou v native-main-repaint-816d7af
a native-main-repaint-minimum-816d7af, PID21892/44000; jde o skutecny
client runtime, nikoli bitmapovy mock. Predchozi failed styled roots
zustavaji odlisene a nevydavaji se za corrected-source PASS.

MSAA original logo/list/body/recovery/action names jsou explicitni,
vlastni UI-thread COM apartment je vyvazen a annotations se uvolni
pred HWND. Actual UIA uz nepopisuje logo nahodnym Recovery textem.
Dynamic state values zustavaji vystavene jako vlastni text, nejsou
prekryte statickym jmenem bez hodnoty. Status/section/hover aktualizace
zustavaji zive a pouzivaji existujici UI tick.

Pure font/layout/raster testy proveruji96/120/144/192/288DPI, original
palette/Segoe roles, selected underline/row a obnoveni vybranych GDI
objektu. Separate20 warmup+1000 draw/create/drop cycles bez HWND,
cursor query nebo jakychkoli physical ports: GDI4->4,1.04s.
Cela workspace259 regular testu a Clippy PASS pred poslednim coupled
repaint fixem; nasledne vsech66 native regular testu a Clippy znovu PASS.
To neni whole-GUI hour ani whole-client memory dukaz.

Zbyvajici explicitni rozdily: extra main quick-action toolbar/separate
Settings/optional read-only body, native checkbox/tab/text raster metrics;
Recovery ma zatim puvodni raw jednoradkovou prezentaci misto vsech WPF
detailu/empty status a SignedIn header zatim obecny text misto emailu.
Full visual requirement se proto NEUZAVIRA. Actual main na jinych
monitor DPI, human keyboard/screen-reader, full controller/physical,
installer upgrade a private Azure akceptace take nejsou odvozene
z techto synthetic nahledu. Frozen7b performance cisla nadale patri
jen frozen7b; relevantni nove mereni musi mit vlastni binary/source.
Finalni oba owned preview procesy21892/44000 uz skoncily normalne,
synthetic files jsou byte-exact a token cache chybi; closed hashes
e9d1b406e729f5d1135a7ff692fbf2382e1a53d0e29028976183783cb0c658ca
a e96525105a54586a4f438a39580bdb2c396218ccac8292736f1f4e9ed8bcfb2e.
Current release znovu sestaven,7 actual C#/Rust platform/DPAPI/IPC/
original-PE-icon parity groups PASS, reference build zero warnings.
Zadny clipboard/input/device/account/cloud port ani dalsi physical
guarded-paste case se v tomto inkrementu nespustil.

### Recovery/account presentation a neblokujici auth display

Navazujici inkrement doplnuje puvodni Recovery obsah: local updated time,
sealed pending chunks/tail vysvetleni, language a Cloud audit upload pending
True/False. Pouziva skutecny NativeRecoveryInfo ze skutecneho encrypted store,
ne staticky screenshot mock. Normalni radek ma4 linky/original selected
styling; maly viewport dve kompaktni linky se stejnym datum/language/pending/
audit obsahem, plny text zustava v accessible row name. Neudeluje to
Recovery automaticky paste. Samostatny footer rozlisuje recovering/no-paste,
complete-to-History, stopped/retained a failed; jeho compact verze se
prizpusobi geometrii pri resize. Recovery origin je explicitni view flag,
zachovany behem progress a resetovany pri nove live generation. UI labels
jsou puvodni Recover to History/Refresh/Discard selected.

Empty History ma puvodni48h hint. Actual image kontrola odhalila, ze
prvni implementace mela text jen v AX tree, ale opaque empty ListBox ho
prekryval. Empty list je ted skryty pouze pri empty/hint surface a vraci
se pri nove historii; nejde o pouhy accessibility-proxy PASS. Actual min
Recovery navic odhalilo1pixel presah compact row a offscreen selected
row po resize; compact46DIP se do actual70px144DPI viewportu vejde,
zmena item height zachova/revealuje selection a neprovadi se zbytecne
pri kazdem layout. Prilis dlouhy compact footer je nyni strucny.
Predchozi cases bez techto oprav zustavaji zachovane jako failed visual
evidence, nikoli corrected-source PASS.

Fresh normal/min actual cases native-recovery-detail-f93003c-b a
native-recovery-minimum-f93003c-b pouzily frozen
cc41381de045bf31e5595250ea97ab6cbd138c27845db92d6d469b5e1d481921,
PID54668/31764. Skutecny CLI journal vytvoril pouze synthetic zero PCM
legacy+audit fixtures v oddelenych root/IPC; mikrofon nebyl otevren.
Actual image/AX prokazaly viditelny empty hint,4-line normal a2-line
min Recovery, selected row, visible compact footer a scroll/select
auditTrue -> auditFalse. Nebyl stisknut Recover, Refresh, Discard,
Copy, Save ani Sign in. Normalni bound exit, unchanged settings/history,
noToken a nezavisle byte-exact SHA vsech encrypted Recovery files;
closed c7ca07d7afe1f42b1a4c759020dd3923b701700cb2620eeaaf4a4e689bc08831
a 121d21a64fd00211d9d9c623ba0acad8291e54a8e64210c61637c987f66f49c8.

Header ted umi puvodni account email a spravne sign-in/out actions,
vcetne SignInNeeded local clear a busy states. AuthRuntime publikuje
state+display email jako jeden snapshot po persistence, jeste pred
state notification. Jeho UI cteni je synchronni/neblokujici: browser/
network/cache session lock ho nesmi zastavit ani zablokovat Sign out
command loop. Prime controlled blocking-store race overuje dostupny
display pri drzenem session lock a odstraneni emailu po sign-out.
JWT decode zustava jen display/scheduling, ne signature verification;
backend Google bearer ani jeho revocation/cancellation invariants
se nemeni. Auth-only report nadale neuklada account/email/token content.
Zadny novy skutecny login nebyl spusten; live SignedIn UI neni z teto
synthetic coverage vyvozena. Settings uz nelze mylne oznacovat jako
isolated pilot ani v installed rezimu; popis je neutralni pro oba.

Cela workspace264 regular tests,71 native, Clippy/current release,
7 actual C#/Rust platform/DPAPI/IPC/original-icon parity groups PASS.
Separate1000 no-window GDI cycles znovu4->4 (0.86s). Predchozi f93003c
RustCI41505134591 vcetneWindows/Linux/MSRV1.85/container a WindowsCI
41505134887 PASS; tento dalsi inkrement potrebuje vlastni CI/current
performance dukaz. Whole visual/human/controller/install/privateAzure
acceptance se stale neuzavira a zadny drivejsi physical consent
se nerecykluje.

Sestaven byl take current build-only Inno installer, nikdy nespusten/
instalovan:976cd0a39e70fce2f70ca4b405d2d56e3dcbf74266481134d6768db38e68ef86.
Staged VoicePrompt.exe byte-exact odpovida release
b2b867991907059b55364a4b4338ecda111162d698c14302f15b7d37b4d04d18.
Dalsich6 existing isolated native Settings/client lifecycle groups PASS.

### Current-main performance: zachovany incomplete failure a oprava eager UI

Exact frozen a2d1200 klient (releaseB2B86799...) byl srovnavan s current
allowlisted C# bundlem,30+30 starts/3 paired301-point idle series a stejny
200-entry corpus. Prvni invocation selhal v caller preflight kvuli
neexistujicimu output directory PRED prvnim client procesem; logs zustaly.
Po oprave pouze tohoto path precondition provedl fresh collector b
16 starts kazde implementace a skoncil na dalsi readiness>15s.
Zadne memory serie ani kompletni30-trial verdict nebyly dokonceny.
Toto je FAILED/INCOMPLETE, nikoli PASS. Native observed starts obsahovaly
1.432s/1.590s/2.190s/10.306s, C# take5.024s/12.452s. Puvodni stdout/stderr
a32 log-recovered rounded0.001ms observations jsou zachovane v
client-comparison-main-a2d1200; nelze je vydavat za full-precision raw
nebo vybrat jen rychlejsi subset. Vsech32 logged owned procesu skoncilo
a generated state roots byly uklizeny. Pricina timeoutu nebyla
prokazana. Pozdejsi current aggregate hostCPU72% neni dukaz drivejsiho
load ani omluva pro automaticke snizeni cile.

Source audit prokazal konkretni zbytecnou odchylku: native hidden main
pri startup formatuje200 local date/history rows, zatimco actual WPF
MainWindow je vytvoreny az na Open. Native ted odlozi pouze presentation
work do visible window; vsech200 history entries zustava v plnem modelu
i durable store, recovery/streaming/outbox/tray/delivery runtimes zustavaji.
Hidden tick nadale zpracuje clipboard/notice/dictation a tray state;
WM_SHOWWINDOW invaliduje presentation, takze nove Open neni stale cache.
Nezahazuje se audio, historie ani funkce kvuli pametovemu benchmarku.
Explicitni self-test stale force-renderuje sve hidden control probes;
coupled6-group lifecycle regrese zachytila a overila tuto vyjimku.
Tato oprava neni tvrzenim, ze eager formatting byl jedinou pricinou
predchoziho oboustranneho zpomaleni.

Fresh NoPorts actual UI native-lazy-main-a2d1200/PID55824/
frozen225c97fa97b88f5a5d32822894d232e9759203e5f5b2d457b97a5207c889ef82
prokazal200 retained entries a presne0 hidden formatting calls pred
Show; actual image/AX po Show zobrazily original live UI a200 cache.
Normal bound exit zaznamenal617 visible formatting calls, nezmenene
settings/history a noToken; closed
3ebf47e24e66777da67b9ac541013a3f424ac08a12af41b73323c8a736d9589a.
Zadny clipboard, microphone, input, account nebo cloud port se nepouzil.

Collector ted po kazdem measured startu ulozi immutable plnou presnost
startup sample mimo timed interval a pri chybe explicitni FAILED/
complete_profile=false record; exception zustava thrown. Owned impl/PID/
elapsed/safety budget jsou v error notes pred cleanup. Reuse existujici
complete/partial evidence se odmitne bez pridani failure k hotovemu reportu.
8 fixture guard groups vcetne actual composition roots,71 native tests,
Clippy/release/Ruff a6 lifecycle groups PASS. Functional single starts
se NEPOUZIVAJI jako p95/performance verdict. Default15s safety budget
i1s/70%/50%/80MiB acceptance prahy zustaly nezmenene. Dalsi plny profil
musi patrit opravenemu novemu source/binary, nikoli tiche opakovani
stejneho a2d ve snaze vybrat lepsi vysledek.

a2dRustCI41510743527 mezitim dokoncilo Windows/Linux/MSRV1.85/container
PASS. To neuzavira performance, human/controller ani celou migraci.

### Opraveny Windows profil stale incomplete; bez dalsi tiche serie

Novy frozen3f4d0ee release
2f6ebd48fb3214bc4282c28b9325ed941d879350e30bf34b3380bc386ef25458
se stejnym C# bundlem prosel startup10Rust/9CSharp. Dalsi C#PID55000
prekrocil puvodni15s readiness budget (15208.243900ms). Zadne memory
serie ani kompletni30+30 verdict: FAILED/INCOMPLETE, zadne PASS.
Tentokrat se zachovalo vsech19 immutable full-precision samples a
failure metadata8aecc4b173358994fae53fc260a3fb24d16b35e1486ab3c7d7f89af5d4c33ed1,
source3f4d0ee. Rust mel i3.740s/5.469s a C#7.819s/12.254s.
Oboustranne zpomaleni neni zpetne pripsane OS nebo jedne platformni
operaci bez dukazu. Vsechny owned client procesy absent, zadny
temporary state root nezustal. Soucasne3f4RustCI41514148488 Windows/
Linux/MSRV/container PASS, coz tento performance failure nenahrazuje.
Neprobiha dalsi automaticka serie. Koordinator dostal nejmensi potrebnou
podminku: klidne merici okno na TOMTO schvalenem pocitaci, nikoli novy
profil/stroj; acceptance targets i safety budget zustavaji nezmenene.

### Actual Azure-adapter HTTP performance: source/CI fixture, vysledek pending

In-memory Linux HTTP profil zustava oddeleny. Novy opt-in
loopback-fixtures build example cloud_contract umi skutecne
Repository/Blobs/Queue Azure adaptery nad verified-HTTPS127.0.0.1
Azurite; default production binary/config/image graf zustava beze
zmeny a zadny emulator/shared-key fallback se do nej nevklada.
Fixture vyzaduje explicitni marked root, private synthetic identity
header/exactloopbacktoken endpoint, own CA cert a random32hex namespace
pro KAZDY proces/implementaci. TLS/redirect/proxy/API-version guardy se
nevypinaji. Stejny GoogleRS256/JWKS/allowlist workload ma puvodni
Python FastAPI factory se skutecnymi Azure SDK Table/Blob/Queue adaptery.
Zadne osobni credentials, actual Azure nebo paidAI.

Existing pinned official Azurite runner po actual REST/worker/cleanup/
continuous/full-replay kontraktu spusti30 starts kazde implementace a
3x100 matched API flows se stejnym emulatorem, puvodnimi metrics a
1s/50%readiness,50%RSS,110%APIp95 prahy. Namespace create/delete je
mimo timed interval; skutecna adapter initialization/probes zustava
uvnitr process->readiness. API sam attests Azure vsMemory, nezavisly SDK
reopen po kazdem processu overi recording/chunk/transcript count,
canonical audit envelope body a queued work. Startup namespaces jsou
prazdne, obe load namespaces maji301 records/chunks/transcripts.
Cleanup vsech owned names a emulator/identity/privatekey musi uspet,
nez se finalni backend-azure-performance.json publikuje.

Lokalne passed97 Rust cloud regular tests/Clippy/default+feature builds,
all-workspace/all-target/all-feature check a5 Python Azure API guard
groups vcetne SKUTECNE SDK init pres vlastni verified-HTTPS responder
(7 owned requests,onecached scoped synthetic token). Dalsich5 existujicich
TLS/emulator groups a5 authenticated-factory/threshold groups PASS.
Toto jeste neni actual Azurite performance run: localDocker engine
zustava nedostupny, novy LinuxCI vysledek musi byt skutecne stazen a
nezavisle prepocitan. Zadny cloud performance prah se nepovazuje zaPASS
z existence fixture.

Pri sirsim actual HTTP parity runneru byl navic opakovane zachycen
Windows10053 connection reset u nekterych requestu odmitnutych pred
prectenim body (invalidoptionalheaders/encoding); obsahove asserts
ostatnich scenaru prosly. Pricinu jeste overit a opravit, ne ignorovat/
maskovat retry nebo oznacit cely runner zaPASS. Nadale jde o otevrenou
regresi/transportni acceptance mezeru. Zadne nove physical/account
opravneni tento cloud inkrement nepouziva.

### Rejected HTTP body: bounded connection lifecycle oprava

Windows actual HTTP parity opakovane vracelo10053 u nasledujiciho
keepalive requestu po invalid headers/encoding s neprectenym body.
Handlers odmitaji metadata pred body zamerne; validation precedence
se NESMI opravit tim, ze se nejdrive neomezene nacte audio.
Novy internal request-body owner proto drzi puvodni HTTP stream
az do rozhodnuti connection policy po handler response. Uz zpracovane
body nema dalsi drain/copy. Pouze unread body s explicitnim
Content-Length<=64KiB muze byt bounded drain do100ms; chyby/limit/
timeout nebo unknown/velka delka znamenaji viditelny redacted log
a Connection:close, nikoli novy request/retry nebo jiny HTTP problem.
Unknown-length malformed-upload test stale NEPOLLNE audio stream
a nadale vraci vsechny puvodni422 fields ve stejnem poradi.

Direct error/lifecycle/headers parity a novy40x sequence
invalid-upload -> encoded-refine -> authenticated-GET prosly bez
retry/resetu; original pred-fix failed runs zustavaji priznanou
evidenci, ne backend-acceptance PASS. Dalsi unit cases proveruji
spotrebovane/male/stalled/oversized unread body a100ms bound.
99 cloud regular tests/Clippy feature graph a4 external HTTP groups
lokalne PASS; cela workspace/CI a actual Azure profile nasleduji.

Prvni ecd7fdc backendCI41521519937 skoncil na Ruff import classification,
nikoli na emulator performance: local command z repo root nerozdelil
measure_backend a voice_recorder jako backend working-directory CI.
Specific TYPE_CHECKING import boundary je opravena a exact backend
CI cwd lint/format prosel. Predchozi failedCI se neprepisuje jakoPASS;
novy actual profile musi probehnout v naslednem commitu.

### Actual adapter-matched Linux profil: vsechny puvodni scoped prahy PASS

BackendCI [41523050769](https://github.com/tkubica12/cot-voice-recorder/actions/runs/41523050769)
na source33d04aa27c26a1816a41cf8bbd5833545e6e3f46 skutecne provedlo
verified-HTTPS OAuth Azurite REST/worker/cleanup/continuous-replay
kontrakt a actual Azure-adapter HTTP performance. Step20:55:52Z ->
20:57:28Z skoncil success; nejde o pouhou existenci pipeline.
Downloaded backend-azure-performance.json SHA256
4349836d03e61cedd05458554147595c85bdbe90863c4bf8b23aa070f13d7f27
je v azure-profile-33d04aa artifacts. Samostatny verifier bez importu
collector metrics znovu vypocetl vsech60 startup samples,4200 API
observations a60 actualRSS values, kazdou operaci v kazde ze3 serii,
original thresholds a62 SDK-checked/deleted process namespaces.
Jeho synthetic selftest take odmitl missing sample, wrong verdict
a duplicate namespace; selftest se nevydava za actual benchmark.

| Actual HTTPS storage adapter profil | Python SDK/FastAPI | Rust REST/Axum |
| --- | --- | --- |
| Process -> actual readiness p95,30 starts | 809.698254ms | 147.049013ms |
| Mean post-load RSS, serie1 | 108.812500MiB | 18.886719MiB |
| Mean post-load RSS, serie2 | 117.910156MiB | 21.378906MiB |
| Mean post-load RSS, serie3 | 129.207031MiB | 23.480469MiB |
| Nejhorsi Rust/Python API p95 pomer, vsechny operace/serie | baseline | 82.159901% |

Rust readiness <=1s a <=50%baseline, RSS <=50% v KAZDE paired serii
a APIp95 <=110% kazde operace/serie jsou vsechny PASS bez snizeni
prahu. RSS je explicitne short post-load10-point observation, nikoli
hodinovy backend soak ani Windows private bytes. Stejny RS256/JWKS,
allowlist, inputcorpus a persisted workload se opravdu pouzily pro obe
implementace. Namespace setup/delete je mimo timed interval, adapter
init/probes uvnitr. Invalid-auth/model/realtime controls se nepreskocily.

Nezavisle source verification porovnalo vsech66 recorded source/lock
SHA s Git objects presneho commitu, nikoli s working-tree CRLF kopiemi.
Linux runtime binary samo neni uploadovane; jeho hash
b923b8a51c880043ffc456ba308ccf0e05a84c0c6eda4c349c3f876b7752eaf4
je CI record, ne zpetne lokalne prepocitany Linux executable.
Cross-check actual storage contract
56913c9eda2af0addb60c2f940cff4b93f776ad8b4aba618e99ac6b79a06c324
znovu potvrdil SDK Int64/CAS,Blob lease fencing,Queue receipts,
legacy finalization/durable delivery,active-lease cleanup a continuous
single-stream/full replay s audio-retained-until-finalization. Zadny
failure/cleanup-failure artifact, owned emulator/identity stopped
a temporary privatekey removed. Normal production binary/config
stale nema emulator/sharedkey fallback.

Toto uz uzavira konkretni actual-storage HTTP profile gap; NE
whole migration. Neprokazuje real Azure IAM/UAMI permissions,
private endpoints, deployed ACA readiness, model inference/worker/
cleanup/realtime latency, cutover nebo nove Windows measurement.
Windows current3f4 profil nadale FAILED/INCOMPLETE, zadne third
serie/physical scope se tim neautorizuje.

Current build-only installer byl znovu sestaven nad actual current
native3f4 runtime: Setup
0a05d6dc2750c214ab539c6432934d9eee43ec14c6335159b88454d985eca86c,
staged/releaseVoicePrompt.exe oba
2f6ebd48fb3214bc4282c28b9325ed941d879350e30bf34b3380bc386ef25458.
Installer nebyl spusten, instalovan ani publikovan jako release.

RustCI [41523051022](https://github.com/tkubica12/cot-voice-recorder/actions/runs/41523051022)
pro stejny33d04aa source nasledne dokoncilo vsechny Windows/Linux/
MSRV1.85-all-features/container joby SUCCESS, vcetne existing bounded
accelerated capture-volume regression. Ta nadale neni real hour/full
controller acceptance. Zadny local watchdog/measurement process nezustal.

### Dalsi skutecna UI mezera: Settings viewport, scroll a save isolation

Source audit nasel pevne570x550DIP Settings bez resize/scroll/work-area
handling; na male/highDPI plose tedy dolni volby/Save nemusi byt
dostupne. Novy native layout ponecha wide label/input columns a v uzkem
okne je sklada pod sebe, vcetne puvodnich10 fields a nezmenenych
values/JSON. Resizable window ma scrollbar, line/page/thumb/wheel
navigation respektujici Windows wheel preference. Tab dialog routing
odkryje nove focused field bez global hotkey/paste. Scroll je ohraniceny
content/viewport a DPI reflow zachovava logical offset; initial bounds
a suggested DPI rect se clampnou na owner-monitor work area vcetne
negative monitor coordinates. Original embedded icon zustava stejny.
Settings muze designove zustat native/system-font podle vyjimky
uzivatele; original main/overlay theme se nemeni.

Pure tests pokryvaji4 viewporty pri96/120/144/192/288DPI, label/input
non-overlap a reveal vsech interactive rects z ruznych scroll offsets;
separate work-area test kontroluje negative/small monitor rects.
Existing self-test ted SKUTECNE hidden HWND resizuje na570x550 i340x260,
scroll/reveal kazdy z10 inputs iSave a overi jeho native coordinates
uvnitr viewportu, presne values a unchanged foreground/nonvisibility.
Je pridan jako samostatny existing6-group lifecycle krok do WindowsCI.
Nejde o proxy "rect math only" ani human keyboard acceptance.

Actual default144DPI Settings855x825px (PID34448) ukazaly vsechny volby,
original icon a disabledSave. Jediny CUI coordinate drag sice hlasil
done, ale bounds se nezmenily; toto se NEVYDAVA za resize proof.
Fresh minimum fixture PID16216 vytvorila Settings510x390px=340x260DIP
se stacked fields. Jediny indexed scroll zmenil actual scrollbar0->100;
actual pixels potom ukazaly final wait1500, guidance a cely disabledSave.
Indexed Close bez Save, zadne field edits/Enter/clipboard/mic/login.
Obe roots normal bound exit, values/settings/history byte-exact,
noToken a owned processes absent. Frozen/defaultF736dfbf...,
minimum0dc8f749... a closed hashes21942d7f.../4c2177e3... jsou v
native-settings-minimum-33d04aa/visual-observation.json; neni to novy
physical nebo performance grant.

Save ve validation fixture je explicitne disabled/guarded i v UI method
a skutecny ClientHost odmita SaveSettings PRED UI shortcut preflight,
autostart registry a storage. Direct fixture command s auto_start=true
overil refusal a unchanged files/noToken. Normalni nonfixture klient
si zachovava dosavadni Save/validation chovani. Source navic odstranuje
obsolete fixed-y initializer ve prospech jedne geometry definice;
stary scale_children helper zustava jen pro puvodni DPI tests.
Lokalne270 regular workspace tests (74native/99cloud), Clippy allfeatures,
release,7 actual platform/DPAPI/IPC/original-icon parity a6 hidden
Settings/client lifecycle groups PASS. Novy source potrebuje vlastniCI.
Real keyboard/screen-reader/monitor transitions a aktualni statisticky
Windows profil nadale nejsou uzavrene; treti serie nebyla spustena.

Prvni7a0bfd3 RustCI41529528682 narazilo na Ubuntu cargo-fmt zalomeni
jedne DPI offset expression (nikoli runtime/geometry assertion).
Expression je rozepsana na explicitni old/new DPI locals a checked
conversion, se stejnou aritmetikou; local fmt/native74/Clippy PASS.
Tento failedCI se nevydava za uspech; opravny source potrebuje vlastniCI.
Build-only7a installer byl skutecne vytvoren bez spusteni:
b3c0c122f74b2de474d5d12d5e5e163ec051b1d9ae1b374e9c99317ad66787ad,
staged/release native
feaec416a8a8a4e5d7a5ad1fa7de3e8705e3ea5943f88fc42fee3758f078fbbc.

### Overeny konec Settings inkrementu, zbyvajici hranice

Opravny [0713037 RustCI41530436878](https://github.com/tkubica12/cot-voice-recorder/actions/runs/41530436878)
skutecne dokoncil vsech5 jobs SUCCESS: Windows/Linux offline tests,
MSRV1.85-all-features na obou OS a cloud-container. Explicitni actual
hidden Settings viewport/lifecycle step na Windows21:50:52Z->21:50:56Z
je SUCCESS (na Linuxu spravne skipped); Windows job dokoncil take
existing bounded capture checks. Zadna predchozi format failure
nebyla zakryta a zadny watchdog/fixture proces nezustal bezet.

Automaticky backend rerun7a0bfd3
[41529528649](https://github.com/tkubica12/cot-voice-recorder/actions/runs/41529528649)
take dokoncil actual Azure-adapter profil. Samostatne stazeni a
nezavisly prepocet opet overil60 starts/4200API/60RSS/62SDK namespaces/
66 source-lock hashes a vsechny original thresholds PASS:
readiness p95Rust137.427558ms/Python817.270737ms; RustRSS
19.191406/21.628906/23.863281MiB proti109.281250/118.406250/
129.734375MiB; worstAPIp95 ratio82.794933%.
Report7322190d6e0f66d13fb3d23d34907d1e446c780260a37c64626a98725b2d8399
a contract98bf4d66bcb664fff1e05adb1caf973120d5d96ff5d9a42863e1e5d0cec30ff1
jsou oddelene od prvni33d04aa evidence. Linux cloud binary ma stejny
recorded hashb923b8a5... jako33d04aa; Windows-only UI zmeny se tak
neprepisuji na novy cloud runtime. Ani tento druhy scoped PASS neni
realIAM/PE/deployedACA/model/workerlatency/whole-migration acceptance.

Finalni0713037 build-only installer:
729e3805fc51dca46c06bb0a7edef6ff470336198e60d1a371584984e4362f35,
staged/release native oba
76e3b043db1d533a3b17e8946f7b29c69acacb3d4932ccc8aca461cb01c8ade2.
Sestaven, nikdy nespusten/instalovan/upgradeovan ani releasovan.

Aktualne implementovana povolena Settings oprava i jeji CI/NoPorts
dukazy jsou uzavrene. Cele dokonceni zustava NEUZAVRENE: current Windows
statisticke profilya2d/3f4 jsou FAILED/INCOMPLETE, zadna memory serie
nebyla dokoncena a treti serie nebyla schvalena/spustena. Parentuv
dotaz na klidne~40min okno byl prerusen bez odpovedi; samotne resume
nereprezentuje takovy souhlas. Dale chybi human keyboard/screen-reader/
monitor transitions, automatic guarded-paste/full-controller/physical
akceptace, skutecny installer upgrade/rollback a real private Azure/
IAM/cutover akceptace. Tyto hranice nelze nahradit synthetic PASS.
Starsimic/Google/clipboard approvals jsou spotrebovane a45788 je pouze
assisted/manual-CtrlV partial. Zadne nove personal cache, Azure zmeny,
paidAI, install nebo dalsi fyzicky pokus se neprovedly.

### Jedna NOVE schvalena quiet-window currentWindows serie: scoped PASS

Po predchozim nezodpovezenem dotazu bylo polozeno JEDNO nove child
ask_user s presnym nonphysical scope. Uzivatel vyslovne vybral:
"Ano, povoluji jednu serii; nyni je klidove okno" (presna puvodni
odpoved s diakritikou je v authorization.json). Tim se schvalila
prave jedna nova kompletni current30Rust+30CSharp/3paired5minidle
serie na TOMTO pocitaci,200synthetic history entries, puvodni thresholds
i15s operational readiness budget. Resume se za souhlas nepovazovalo.
Po dobu collection nebezely dalsi localbuilds/tests/zatez. Predchozi
faileda2d/3f4 data nebyla mazana, opakovana ani zpetne prohlasena zaPASS.

Current code0713037, checkout86f3b0f, frozen native
76e3b043db1d533a3b17e8946f7b29c69acacb3d4932ccc8aca461cb01c8ade2
a stejny allowlisted C#fixture bundle byly overeny pred startem.
Git potvrzuje posledni App/Core/ClientFixture sourcechangef93003c,
takze frozen reference je aktualni, nikoli jina osobni instalace.
Datum actualserie3.listopadu2026, explicit own roots/IPC, zadny OAuth/
device/hotkey/clipboard/paste/registry/install/backend/paidAI port.
Jeden bounded detachedcollector prezil normalne,exit0,stderr empty,
bez automaticretry. All60immutablestartup observations i6x301memory
samples byly skutecne dokonceny.

Nezavisly verifier bez importu collector metrics prepocetl vsechny
raw60startups+60immutablefiles+1806memoryvalues,nearest-rankp95,
means/maxima,KAZDOUpairedseries aoriginaltargets,actualdurations,
samecorpus/bundlehashes/source/PIDs a cleanup. Report
client-comparison-authorized-0713037/measurements/full-client-signed-out-comparison.json
SHA256f99ab7126e3cf06913e648fa72716cffec22635500006518c144fbb6c8ddaa3e.
Independent-verification SHA256
e2cd14d5787e6397b4378842877a351c7cc2d47d04054d2ecf7cdc627992a395,
verifier vfiles/verify_current_windows_profile.py; jeho vysledek neni
success-shaped fallback ani tvrzeni cele migrace.

| Current signed-out runtime071,30starts/3pairedidle | C#reference | Rust |
| --- | --- | --- |
| Startp95,nearest-rank30 | 1706.136100ms | 257.948300ms |
| Mean private bytes,vsech903samples | 95.183114MiB | 3.427075MiB |
| Observed maxprivate,903samples | 107.859375MiB | 3.992188MiB |
| Paired meanprivate,serie1 | 94.197259MiB | 3.451308MiB |
| Paired meanprivate,serie2 | 96.912765MiB | 3.293423MiB |
| Paired meanprivate,serie3 | 94.439317MiB | 3.536493MiB |

Rust startp95 je15.118858%baseline a<=1s/<=70% PASS. Max observed
private<=80MiB aEACHpairedmean<=50%baseline PASS (Rust3.664/3.398/
3.745% respective C#series). Actual durations299.9802343..300.0137489s,
requested1s/301points, zadny zkraceny/accelerated measurement.
Vsechny66 own measured processes probehly/ukoncily (PIDreuse se
nezamenuje za novy process),zadny own frozen-image process nezustal;
generated state roots odstraneny, original rawproofs/bundles retained.
Source86f3b0f/branch byly po collection stale clean/unchanged.

Tato nova actualserie uzavira konkretni current-full-runtime-SIGNEDOUT
startup/idle scope. NE signed-in backend/visible-window/controller-hour,
physicalpaste/mic/humanaccessibility/installerupgrade/realprivateAzure/
wholemigration acceptance. Predchozi7b cisla nebyla prepsana na tuto
binary a predchozi failed/incomplete serie s neprokazanou pricinou
zpomaleni nebyly ignorovany. Nova one-series
permission je nyni spotrebovana; nedava dalsi repeat ani fyzicky grant.
Zbyva novy explicitni souhlas pro nejmensi physicalDesktopGuard test,
human/fullcontroller a install/privateAzure hranice; guarded45788
stale jenassisted/manualpartial.

### NOVY physical guarded-paste case: safe block, nePASS; checked commit oprava

Uzivatel nasledne NOVE schvalil jeden3min isolated automaticDesktopGuard
test s jednim personalRun,500ms delay,fixedmarker a bezmanualCtrlV/retry.
Fresh case PID50040/window592838/frozenCDD6210947c7da1d290533de834b87828ca9c2b5aaf478b56df8d4d016adfacf
skutecne acceptedRun zaznamenal jeste pred agentprompt. Agent proto
NEZADAL dalsi klik. Immutable focus/armed/delivering/result prokazuji
own-focusHResult0/targetmatched,true a jednou publishedmarker, ALE
typedguard clipboard_sequence_changed:expected25/actual28.
native_emission_invoked=false/native_events0/input_sent=false;zadny
nativeCtrlV se nevyvolal. Nativeown-field markerobservation nebyla
attempted, read-only CUI field byl bezValue aRun disabled. Zadny
manualpaste/clipboardread/restore zagentastrany ani druhyRun.

Normalni3min windowshutdown,processabsent aonlyowned.lnkremoved;
resultab31ccfb3804948256a6c6d1207db48f9f43771b74d37f6c7f8212340b94bf9b,
closedf143f1b34f2f4072d3861bfea223d392b69b4fc8fc877b4244b0bbf454e987b4
afailurec8a8ef004a6961e33f2e23febdd9b35f06f06237fca070805b233fcb80e1ad7b
jsou zachovane ve guarded-paste-authorized-0713037. Tento souhlas je
spotrebovany;NENIautomaticPASS a45788 se nadale nechava assisted-only.
Po sequencechange nelze tvrdit actual currentclipboardcontents bez
zakazaneho read; proto se reportuje jenonepublication/norestore a
fixtureUI uz nepodsouva "marker remains" jako overeny pozdejsi obsah.

Sourceaudit odhalil konkretni orderinggap: write_with_sequence nejprve
vyhodnotilo GetClipboardSequenceNumber a AZPOTOM RAII Drop provedl
CloseClipboard. Actual C#MarkedClipboardWriter volaMarkClipboard
po dokonceneStaClipboardWriter.SetText/flush. Microsoft
[CloseClipboard](https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-closeclipboard)
popisuje dokonceni zmen/umozneni pristupu dalsim windows;
[sequence API](https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-getclipboardsequencenumber)
zavisi na contents/empty/render changes. Tyto source/API dukazy
NEPROKAZUJI zpetne,ze vsechny3liveincrements vyvolal konkretneclose;
OS rendering/external activity zustava mozna. Neni zadne fixed "+3"
pravidlo ani povoleny rebase na aktualni seq pri selhani guardu.

Oprava checkedclose -> sequence -> still-own-owner metadata vraci
commit snapshot az po dokoncene ownpublication, stejneho radu jako
C#reference. NULL/changed owner/zero sequence/failedclose znamena
viditelnoufailure (includingNULLprojectedS_OK asno verifiedowner),
nikoli success. Guard je thread-affine a successful explicitclose
nevytvari double-close; resource cleanup po chybe neni dalsiwrite.
[SetClipboardData](https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-setclipboarddata)
vyzaduje owningwindow/openclipboard, memory se transferuje pouze
poactualSetData success. BezGetClipboardData/contents read/restore.

Prime NoPorts testy pouzivaji STEJNY production commit helper:
simulatedclose meni25->28 a returnedseq je28; failedclose query
nedosahne,zero/lostowner/APIerror se nevydavaji zaCopy success.
STEJNY productionDesktopGuard s synthetic emitterem prijme completed
snapshot28,ale naslednouchanged29 odmitne PRED emitterem. Focus/
modifiers/sequence/once-only/release safeguards se neoslabily.
77native/273fullworkspace+Clippy/release aexisting6lifecycle/7platform
groups PASS bez jakychkoli systemclipboard/input/device/accounts
calls vtechto NoPorts testech. Fyzicky post-fixPASS/novyCI jestezbyva;
predchozi071WindowsPerformancePASS patri frozen76e3... a neprepisuje
se na tuto pozdejsi clipboardbinary. Novyphysicalpokus potrebuje
dalsiNOVE rozhodnuti, ne recyklovany souhlas.

### Checkedclipboardcommit NoPorts inkrement: fullCI PASS

Source5bcaee592b9dad61d1545e4f262676e3ce8306ed byl po273localtests/
77native+Clippy/release/data/lifecycle overeni commitnut a pushnut.
[RustCI41590534884](https://github.com/tkubica12/cot-voice-recorder/actions/runs/41590534884)
skutecne dokoncil vsech5 Windows/Linux/MSRV1.85-allfeatures/container
jobs SUCCESS. NoPorts tests nevolaly systemclipboard/input APIs;
prokazuji checkedclose/sequence/ownerorder a preservedlate-change
rejection, nikoli zpetne successfulphysicalpaste.

Aktualni build-only installer5b:
b2a0e529718cb737bc23ce194d2348cd24e1b74d2106a0bdc370cba0798d9dbb,
staged/release native oba
f01fe1355418f01f817747b215f580e6dcb5798ccf4a2b345f88ce506eb4e7fc.
Sestaven, nebyl spusten/instalovan/upgradeovan ani releasovan.
Mezi NoPorts opravou a timto CI nebylo znovuotevrene actionwindow,
clipboardoverwrite,paste,mic/login nebojinyphysicalpokus.

Nejmensi zbyvajici physicalgate je NOVYexplicitne schvaleny
jeden3min case se stejnym fixedmarker/personalRun/noManualCtrlV/
500ms/atmostoneDesktopGuardbatch/noRetry aownblanktarget; nove
metadataovereni kontroluje onlyownowner/sequence, nectestareani
noveclipboardcontents. Takovy souhlas tedneni kdispozici a neni
odvozenzpredchoziho50040failure/071performance nebo obecnehoresume.
Real automatic/fullcontroller/human/upgrade/privateAzure acceptance
staleNEuzavrena.

### NOVY po-fix ONEphysicalDesktopGuard port: actualPASS

Uzivatel NOVE vybral "Ano,povoluji jeden novy test po tetooprave",
se scope pouzejedno3min ownisolatedtopmost/active-monitorwindow,
personalRun+1shands-off,NOmanualCtrlV,onefixedmarkeroverwrite,
500msguards/atmostoneDesktopGuardbatch,bez retry/Enter/Save/jineappky/
mic/hotkeys/accounts/install/cloud aonlypost-close owner/seqmetadata.
Fresh frozen8ffabe37f5c2fdb16d20e80dc54d45990b12b04a5e4f503212694673f3729479,
PID10776/window8784554,testsource5bcaee5. ActualacceptedRun/focus/
armed/delivering/result opetexistovalypredagentprompt; agent proto
NEZADAL druhyklik. OwnPID/image/title/presentation iimmutablephases
bylyovereny, nejdeorecycle50040.

TentokratactualDesktopGuardnative_emission_invoked=true,
native_events4/expected4/input_senttrue,guard_rejectionNULL,
zadny release-onlycleanup,druhybatchneboretry. Onepublication
prescheckedClose->seq->ownOwner succeeded. Po250ms skutecny
GetWindowText ownfieldprokazal presny approvedmarker;
marker_observation_attempted/observedtrue. Independentread-onlyCUI
navicvratilo exactfieldValue iDocumenttext
"VoicePrompt synthetic guarded paste test 2026",disabledRun aPASSstatus.
AgentnezapsalmanualCtrlV/clipboard ani necetl/neobnovil oldclipboard.
Neprohlasuje current contents poemukonceni,zadne posthocsystemread.

Normalnilifetime3min/closed,file,no failureartifact,ownprocessgone,
onlyown.lnkremoved. Result
693f97615e26f0515966265672f08ac1a65356199370750faf515223679f58f8,
closedf012b1f969fa3cde99f6767e938549e1094a8c3562422143def1a19682801a27,
completeverified-result.json vguarded-paste-authorized-5bcaee5.
NovypovolenyONEtestjespotrebovany;zadnedalsiwindow/opakovani.
Starsifyzicke50040BLOCKED/no-emitter i45788assisted-only se ponechavaji
pravdive,zadne zpetne upravenePASS nebo tvrzenipricinyvsecholdincrements.

Totoje actual scoped ONEautomaticDesktopGuardportPASS. Neprokazuje
celou dictationcontroller microphone/hotkey/capture/stream/durableHistory/
stop/focusselection/clipdelivery pipeline,oba hold/toggle modes/hour,
realGoogle/backend/model,userdestination/accessibility/monitor/installer/
privateAzure acceptance. Current071performancefrozen76e3... zustava
source071,nikolinewscreenshot/toolbinary5b. Cele prepisani/akceptace
se proto dale NEPROHLASUJI zaperfektne/uplne dokoncenou.

### Controller: preruseni asynchronni inicializace pred enginem a mikrofonem

Po poslednim jednorazovem paste PASS byla nalezena konkretni dalsi zdrojova
mezera. `DictationHost` cekal na auth/recovery inicializaci a pote bez kontroly
cekajiciho Stop, Discard, Interrupt, auth-change nebo shutdown spoustel session
engine a mikrofon. Tyto udalosti zpracoval az v recording smycce. C# reference
ma synchronni start na UI dispatcheru; Rust asynchronni predel potrebuje vlastni
kontrolu platnosti. Jde o opravu controlleru, nikoliv dalsi fyzicky test.

Priprava journalu a backend adapteru je nyni oddelena od spusteni session
enginu. Stejny produkcni gate kontroluje pending udalosti pred inicializaci
a po jejim dokonceni; engine a mikrofon se spusti pouze v `Ready` vetvi.
Jiz rozpracovany blocking journal worker se dokonci/joinne, neodpoji se s
nepozorovanym zapisem. To neslibuje preruseni blokovaneho OS/storage syscallu.
Stop/Discard odstrani pouze novy, jeste nespusteny journal po uvolneni vlastnictvi.
Preruseni Recovery nikdy nesmaze existujici journal/audio. Chyby inicializace
a uklidu zustavaji explicitni; auth/shutdown/interruption obnovi idle/busy stav
a viditelne vysvetli, ze mikrofon nebyl otevren.

Sest novych deterministickych NoPorts skupin overuje vsech pet druhu preruseni
pred a behem inicializace, dokoncenou/joinnutou pripravu bez prechodu do startupu,
stary generation Stop, oba mode vstupy, closed watch/command channels,
auth epoch i pri stejnem SignedIn stavu, already-seen shutdown, chybove vysledky
a novy-versus-existing Recovery v obou formatech. Existujici synteticke Recovery
maji take skutecny encrypted PCM checkpoint; po preruseni sedi cely encoded
manifest i presne WAV bytes. Zadny test neotevre mikrofon, systemovy clipboard,
SendInput, hotkey nebo account/backend port.

Lokalne po oprave: 13 focused dictation testu, 279 workspace all-feature testu
(83 native), all-target/all-feature Clippy s `-D warnings`, fmt a release PASS.
Dale 6 actual-hidden lifecycle a 7 C#/Rust DPAPI/IPC/PE-icon parity skupin PASS.
Prvni compile odhalil presun String z Drop-protected manifestu a chybne pouziti
PartialEq na tomto typu; opraveno clone ID a porovnanim celeho encoded manifestu
pred uspesnymi behy. Zadny neuspesny beh neni vydavan za PASS.

Novy build-only instalator ma SHA256
d836cf6e1b4bcb48593c302d5e1abf61e27f8e1ecd745a95a439a15b1dc0632d;
release a staged EXE oba
d35852bb0b18430315014d4000efbb65c94794e224fcd542a0409ca2ce786711.
Nebyly instalovany ani pouzity k dalsimu fyzickemu scenari.
Current071 performance a posledni fyzicky paste5b zustavaji presne dukazy
svych puvodnich binary; tato nova oprava je neprejmenovava.
RustCI41596155218 nad code2349319c23201c2a0d09cb9ce0210796785cd244
nasledne skoncilo uspesne ve vsech peti jobech: Windows/Linux offline checks,
Windows/Linux all-feature MSRV1.85 a cloud-container build bez nasazeni.
Windows hidden Settings/lifecycle, DPAPI/IPC/ikonove a v1-v4/text/checkpoint
parity, oba GDI resource testy a NoPorts focus proof prosly.
Windows job dokoncen 2026-11-03T07:40:15Z; watcher skoncil exit0.
Jeho bounded synthetic engine smoke trval 238s, nikoliv novou real-hour
nebo full-controller akceptaci. Zadny aktivni helper nezustal.
Full-controller hold/toggle/hour, human keyboard/screen-reader/monitor,
upgrade/rollback a real private Azure/cutover akceptace stale nejsou uzavrene.

### Synteticka kompozice cele controllerove smycky a keyboard-only NoPorts guard

Nove private typed ports zpristupnuji skutecnou `DictationHost::run` smycku
deterministickym testum bez fyzickych zdroju. Bezny klient vzdy vytvori
`NativeDictationPorts`: stejny authenticated ApiClient, puvodni binding kontrola,
Refiner a Microphone start/finish. JSON/CLI nemohou zvolit mock porty.
Session enginy, encrypted journal, History a stop/polishing/delivery logika jsou
v kompozicnich testech skutecne produkcni implementace. UI reply Pasted/CopiedOnly
je synteticke potvrzeni handoffu, nikoliv dukaz Windows Ctrl+V.

Kompozice pokryva oba rezimy a enginy, presne queued+stop-tail PCM, durable
History pred UI handoffem, original/final text pri polishing success/failure,
Discard versus interrupt/auth/shutdown retention, capture start/stop failures,
chybny History zapis, Recovery bez automatickeho paste, provisional+failed
fallback, joined cancellation blocked completion, closed UI a disabled Settings/
signed-out startup. Nove acknowledgement fixture testuje prubezne confirmed
prefixy; ACK/provisionalni text se nepouziva jako audio release ani finalni History.

Prvni accelerated115200000-byte volume pokus FAILED po645.29s na5s testovem
UI-handoff timeoutu. Jeho original log/binary a jediny overeny synteticky temp
root byly zachovany v controller-volume-failed-37deb3f, neprepsany na PASS.
History uz obsahovala synteticky finalni text, ale handoff/cleanup PASS pro tento
beh neni prokazan. Puvodni fixture krmila nepretrzity high-energy signal bez
quiet interval; produkcni sender tak neprovedl prubezne manual commits.
To neni zdravy checkpoint workload pouzity drivejsim hodinovym testem.
Opravena fixture pouziva explicitni low-energy deterministicke PCM a potvrzuje
jen skutecne committed prefixy. Sample-volume cil115200000bytes ani stop/paste
guard/budgets nejsou snizeny. Real-hour/50min rotation nebo GUI performance se
tim nenahrazuji. Failures zustavaji zachovane, novy velky beh se overi oddelene.

Uzivatel NOVE schvalil jeden3min keyboard-only NoPorts main/Settings test:
Tab/ShiftTab/arrows/Space, cteni a navigace History/Recovery/Settings, Cancel
bez Save; bez CtrlC/V/Enter/send/device/globalhotkeys/account/backend/install/
OS/Narrator/personalstate. Kontrola pred otevrenim odhalila ze stara visual
fixture mela Save/copy blokovane, ale Pause/Clear/Delete/Recover dostupne.
To bylo opraveno pred jakymkoli otevrenim keyboard case: UI controls disabled,
forced command guard pred dialogy/shortcuty a runtime allow-none command gate.
NoPorts `DictationUi::recover` take odmita pred zmenou generation/busy a hotkeys.
Runtime test posila14 ruznych akci a overuje exact Settings/History/Recovery,
nezmenenou auth/policy, prazdne UI requests a zadny token soubor.
Keyboard readiness kontroluje skutecne IsWindowEnabled pro vsechny action IDs;
povolena zustava jen Settings navigace a jeji Save nadale zakazuje UI i runtime.

Dosavadni lokalni native regression:99 regular tests PASS (13 ignored samostatnych
physical/volume/visual vstupu), all-target/all-feature Clippy/fmt/release PASS.
Nove kompletni workspace all-feature regression295 regular tests PASS,
13 actual controller-composition skupin PASS,6 hidden lifecycle a7 C#/Rust
DPAPI/IPC/original-icon parity skupin PASS. Count-only acknowledgement smoke
overil1280000 exact sample bytes, live confirmed prefixes, stop/history/audit
a joined cleanup; nejde o hodinu. Novy build-only setup
1d4c91165858b4be55e68b76a9fb56cca5109f9fd883683a05dadf2ea2588223
obsahuje release/staged2cadaa327c2f14f1ac02696686b8b30ce757228ae95c7c465b7b62cb7ec2654c.
Zadna instalace, skutecna device/copy akce nebo produkcni zmena neprobehla.
Schvaleny keyboard countdown jeste NEZACAL a zadne keyboard okno nebylo otevrene.
Opraveny presny build, dalsi parity/CI a pripadne novy volume dukaz budou
zaznamenany pred dalsimi tvrzenimi o akceptaci. Cela migrace stale neni uzavrena.

Nasledne presnyffde6d3 RustCI41603157120 skoncil ALL5JOBSSUCCESS,
Windows2026-11-03T08:41:06Z. RESTrun/jobs a existujici watcher nezavisle
potvrdily stejny head a vsech5success jobu; watcher exit0/no active helper.
Opraveny count-only controller volume na frozenD41...take PASS:
115200000captured/streamed bytes, kazdy sample byte overen, durable History/audit,
joined capture a skutecne uklizeny ownroot,272.9983441s. Rawlog
07f2b60bf8fb24eaee3d274ba9edca14c94692dcb3bf988f44e8097854bae952
a controller-volume-ffde6d3-result.json zustavaji samostatne od prvnihoFAIL.
Je to accelerated sample volume, nikoliv real-hour,50min rotation nebo nativeGUI
memory proof; predchozi actualhour a current071 evidence se neprejmenovavaji.

Jeden schvaleny manual keyboard case otevrel47232/window3804432,
frozenD41.../actualready144DPI/History3/Recovery2/actionsdisabled. User mel
nejprve pozorovani ze okno vidi, ale po jedine child otazce odpovedel:
"okno mezitim zmizelo,myslim,ze tohle mas delat sam pres computer use".
Actualclosed/processgone, vsech8baselinefilesSHAexact/noToken, onlyownlauncher
removed. ReadyD2EF09.../closed8143CB... zustavaji; keyboard/humanPASS z toho
NEPLYNE a prvni case se neopakuje jako jeho pokracovani.

Nove user-directed prevzeti pres Computer Use autorizuje jeden nezbytny dalsi
automatizacni NoPorts case se stejnymi UI/runtime guards, osobni stav/clipboard/
device/backend/OS/install stale zakazane. Test-only automation entry ma fresh
single-native-keyboard-automation-fixture marker a bounded600s lifetime,
nikoliv dalsi2min lidsky zavod. Bezny klient/puvodni180smanualcase se nemeni.
Nove native99regular/fmt/Clippy PASS; automatedcase bude overen z actualCUI
focus/control/selection/text/SettingsCancel/files, ne jako screen-reader nebo
lidska accessibility akceptace.

Automatedonecasec5d3117/PID31332/main527654/Settings5310788 probe skoncil
BLOCKED pro keyboard dispatch, ne keyboardPASS. Prvni Tab byl serverem prerusen
detekovanym userinput; probe provedl povinny reread a jediny stejny retry.
Tab na Main, Right na tabgroup, Down na History, Space na checkboxu a Tab na
Settings BackendURL hlasily done, ale ocekavane focus/selection/value zmeny
nebyly prokazane. ShiftTab uz nemel target po bounded closure. Zadny shell,
WM nebo SendInput UI workaround ani vstup cilene jine aplikaci se nepouzil.

Indexed CUI pritom skutecne zmenilo History selection na druhy radek,
show-selected-text off->on/exact read-only "Synthetic keyboard preview two.",
Recoverytab/row2 a scroll0->100; Settings ukazalo vsech10ocekavanychhodnot
a disabledSave. Tyto pozitivni UI doklady jsou parcialni a nejsou nahradou
nefungujiciho keyboard dispatch. AgentCancel neni prokazan; normalni bounded
ownerclose zahodil neulozeneSettings. Procesgone, vsech8baselinefilesSHAexact,
noToken,onlyownlauncherremoved. Ready709226.../closedF915F6... a
keyboard-automation-authorized-c5d3117/result-summary.json jsou zachovane.
Grant spotrebovan, zadne dalsi okno nebo genericky human permission kolotoc.
Nezavisle hiddenNoPorts control/layout/save/data tests zustavaji PASS, ale
neni z nich fake keyboard/human/screenreaderPASS.

Presnyc5d3117 RustCI41606601832 take ALL5JOBSSUCCESS, Windows dokoncen
2026-11-03T09:09:37Z. Nove test-only600s lifetime nezmenilo bezny klient ani
puvodni180smanualcase. Cela migrace stale potrebuje opravdove full-controller/
GUI-hour, keyboard/screenreader/monitor, upgrade/rollback a privateAzure dukazy;
zadne z techto neprovedenych hranic se neuzavira timto scoped CI/volume/UI vysledkem.

### Priprava aktualniho controller wall-clock hour, ne stareho engine-only testu

Koordinator zadal dalsi konkretni povolenou synthetic akceptaci: jeden skutecny
3600s wall-clock beh AKTUALNI controller orchestrace s izolovanym stavem a fake
porty, raw warm/capture/postidle private/working-set/CPU, sample-exact poradi,
confirmed cursory/rotation/normalstop/returnidle a puvodni<=10MiBpostidlegrowth.
Nejde o novy30+30quietprofile, physicalhour nebo produkcni souhlas.

Novy typed-port fixture pouziva skutecny soucasny DictationHost.run, streaming
session, DPAPI journal, atomic History, polisher a periodic AuditOutbox engine,
synthetic20ms/monoPCM16/16kHz a dva Tokio worker threads. Worker controlleru i
outbox zustavaji v procesu i pro stejny post-idle; declaredbaseline ma prazdnou
History/Recovery po overenem warmup. Real50min timer zustava beze zmeny. Kazdy
vzorek se porovnava s absolutnim97-sample vzorem, potvrzene cursory musi rust a
capture/transmit/confirmed bytes musi byt115200000. FakeUploader nenahradi realIAM/
Azure; pouze proveruje skutecny outboxack+fixeddurableHistory/noautopaste flow.

Renderer, main/tray UI, globalhotkeys, physicalcapture/clipboard/SendInput,
skutecnyOAuth/PubSub/backend/model/OS/registry/install/Azure se v teto kompozici
NEVYTVAREJI. CUIkeyboardBLOCKED a predchozi indexedUI dukazy se neprepisuji.
Novy report ma current_controller=true/renderer=false/full_client_profile=false,
nikoliv full-GUI claim. Collector uchovava rawreports/progress/stdout/stderr;
finalcomplete report se publikuje az po process a ownedcontrolroot cleanup.
Original10MiB growth threshold neni snizen. Smokes budou incomplete; real-hour
PASS se tvrdi jen pro plny3600s/301pre/301post shape a skutecne rawdukazy.

Lokalni smoke3s/3pre/3post na aktualnimtestbinary84fd1292...prosel sample-exact
96000bytes, History/audit, capture/outbox joins, empty-state returnidle a cleanup.
Jeho afterprivategrowth602112bytes neni hodinova metrika; verdictincomplete.
Nove observerguards4groups (engine/controllername,zerotest,pipefail)PASS,
native99regular/Clippy/fmt a nezavisle6hiddenlifecyclegroupsPASS. SkrytySettings
selftest navicprosel40skutecnych forward/backwardGetNextDlgTabItem dotazu
na10poli/dvaviewporty/disabledSave, bez inputu nebo aktivace; nenahrazuje CUIkeys.
Priprava skutecneho jedinehohodinovehobehu pokracuje, zadnyhourPASS jeste neni.
Collector pro tento controller scope take uklada create-new durablyflushed
JSONL header+kazdyprivate/working-set/CPU/thread/handle sample. Finalrawreport
musi exactmatchvsechnyjournalrows, aby externalinterrupt neztratilcelecapture
telemetry. Puvodni samplery bez optionalSampleJournalPath se nemeni. Novy3s
journalsmoke prosel identity/shape/exactrawrows/cleanup, staleverdictincomplete.

### Aktualni controller houra10c336: FAILED/INCOMPLETE, bez opakovani

Jeden povoleny actualhour nad frozen84fd1292.../sourcea10c336 byl spusten
a ownPID31964/readiness overeny. Warm301rawsamples byly dokoncene; capture
skoncil pri source try_send Full(..) na boundedqueue a proces neziskal normalni
stop. Sampler spravne odmitl completecapture report. Zachovano2837captureJSONL
rawrows (lastelapsed2836.0344511s),301warmrows, immutableprogress a
encryptedfailure-state/stdout/stderr/failure.json. Lastprogress2820.0202126s:
90240000produced/90176000saved/90176000confirmed/90239360provider bytes,
lastrecordedmaxproducerlateness941.9132ms. Lastsampleprivate5353472/WS27803648.

FailureSHA428eed16997a0fe62a093d22b890037e985b505ec0cf2b3f64e00335dad751c0,
warmFA19D86.../capturejournalE110906... a partial-result-summary.json jsou
zachovane. Ownprocessgone/controltempremoved; failure-state se nemaze.
Neni3600s,real50minrotation,postidle ani10MiBPASS a nic se nere-labeluje
ze starehoenginehour/071 profilu. Druhyactualhour NEBYL spusten.

Zdrojova kontrola po selhani overila konkretni framing mismatch fixture:
fakecapture640B/20ms a50queue slots=1sbuffer, nativeMicrophone ma1280B/40ms
a stejnych50slots=2sbuffer. To a Full jsou prokazane; presny podil hostscheduling/
controllerIO na tomto selhani se z dostupneho progressu netvrdi. Oprava testports
sdili puvodni native1280B/40ms/50slots constants; hodnoty produkcniho capture
ani kapacita se nezvetsuji. Overflow zustava explicitfailure, ne drop/success.
Producer pred panikou nyni ulozi ownfailure kontext/frameindex/due-lateness/
queue config a joinne controller/capture, aby nebyl maskovan backend/driver fail.

Novy NoPortsunit naplni50native-sized packets,51.odmitneOverflow,overi vsech
64000queuedbytes a znovupouzitelnost po drain. Native100regular/Clippy/fmt PASS.
Pouze bounded3s/3+3rawjournal smoke po oprave prosel96000sampleexactbytes,
History/outbox/joins/cleanup; verdictincomplete. Neni opravenyhodinovyPASS.
Dalsi realhour je konkretni samostatne rozhodnuti po tomto retainedFAIL,
ne automaticky retry nebo genericka human/device otazka.

### Druhy actualcontrollerhour ed760: FAILED/INCOMPLETE a blocking-persistence oprava

Koordinator konkretne zadal jeden NOVY correctedsource beh po ed760CI41628312416
ALL5success. FrozenoracleE2E34D.../sourceed760/readinessPID60204 overeny,
native1280B/40ms/50slots zachovany. Beh presto skoncil po128.8987879s na
producerOverflow,frameindex3205/4102400prijatychbytes/lateness698.7898ms.
Lastimmutableprogress120.0812945s/maxrecordedlate1723.9439ms;
301warm+128capturerawrowszustavaji,postidlezadne. Ownprocessgone a
failure-state/controllerlogs/producercontext/rawjournalsretained. Dalsi
fullhour automaticky NESPUSTEN a zadny3600/rotation/10MiBPASS z nej neplyne.

Read-only vlastni encryptedfailurestate nasledne overena: capture_complete,
captured4102400 (vsechny prijatebytes), confirmed3968000, puvodni synthetic
backend/accountbinding exact. Vsech134400neconfirmedPCMbytes overeno sample-exact
proti absolutnimu97-sample vzoru; ciphertext soubory pred/po probe byteexact.
Failure2E4D31.../producer18FEC3.../journalD2FFBF.../preservationCF448D... a
controller-hour-ed760da/partial-result-summary.json uchovane. Tato ochrana dat
neni dokoncena hodina; stary a10FAILED/enginehour/071/5bport/CUIBLOCKED se
neprepisuji ani nespojuji do novehoPASS.

Zdrojove prokazana dalsi vazba: DictationHost volal Session.append a finalseal
synchrone s DPAPI/diskem v Tokio captureselect; enginepumpy navic blokovaly
state mutexy. Puvodni capturequeue se nyni cte jednim joined blockingworkerem.
Nevznikla druha fronta, kapacita50 se nemenila, Overflow zustava error a
stopbudget2s/reserved250ms se neprodluzuje. Stop/Esc/auth/shutdown zustavaji
na asynccontrolleru; worker vlastni session az do drain/seal/join, explicitDiscard
jediny invaliduje queuedaudio. Finishing status se publikuje pred cekanim na
durablejoin; kontendovane enginestate locks cekaji kooperativne, ne blokuji
asyncworker. Taskabort uz v legacycleanup neodpoji rozpracovanyjournalwrite:
networkwait se zrusi prescancelwatch, I/Oworker se dokonci pred uvolnenimowner.

Nove deterministicke NoPorts dukazy: current-thread timer/controlmessages pri
blockedpersistcallback, okamzita errornotification pred sourceclose a zadny
retry dalsichwrites; explicitdiscard/preserve poradi; skutecny per-root writegate
na obouengines, plna originalqueue/51.packetOverflow a Stop/Discard/Interrupt/
auth/shutdownresponsiveness, presne accepted/savedbindingbytes; blockedfinalseal
timer i latecancel bez UIhandoff; lockedmanifestfilefailure a durableprefix
bez fakecapturecomplete; legacyshutdown musi cekat na jizspustenyjournalwrite.
Vsechny testports/writegate jsou pouze cfg(test), produkcniDPAPIformat unchanged.

Real storage writefailure nema zaruku durablezapisu neulozitelnychsamples:
test zde overuje ulozenyprefix/binding a explicitni error, ne fakeallcapturedPASS.
UI tuto hranici nowvyslovnehlasi. Pokuddiskneumoznizapis, pouzeexistingRecovery
je prokazane durable; sourcefix nevydava memory/queue admission za ulozeneaudio.
Nove local303allfeatureworkspace/107native/fmt/Clippy/releasePASS.
Prvni coupledregression odhalila jizpotvrzenychunk/nativemarker"" a odpojeny
legacywrite pri shutdown; assertion byla opravena na skutecnyconfirmedtext/
sampleexacttransport a legacyownershipbug byl opraven, ne skryt relaxedtestem.
PresnyOS/scheduling/IOpodil na historicalOverflow stale neznamy. Nove
wall-clockhour/performance/realdevice/fullGUI dukazy stale potreba; zatim pouze
boundedNoPorts validace, nikoliv automaticky treti hourlyrun.

Aktualni6hidden lifecycle a7C#/Rust DPAPI/IPC/original-icon parity groups take
PASS. Pouze3s/3+3rawjournal smoke po rootfix prosel96000exactbytes/durableHistory/
outbox/capturejoins/cleanup; verdictincomplete, nikoliv hourthreshold.
Build-only setupc3b6c567822ae7b1aeff45adf30f36c3ace9b8ead4058538ea4e7e87835cd874
obsahuje release/stagedbefc0abdfafcb67ad59b50686f0aa950f868669a615f3af05cf19889348766c3.
Nebylo instalovano ani pouzito pro novyphysical/hour/scopedstartupprofile.
Presne noveCIbudeovereno samostatne; olded760CI/071memory metrics se na tuto
opravunere-labeluji.

### Treti samostatne zadany controller hour: FAILED, dalsi jen bounded diagnostika

Presny `4430f8e` prosel vsemi peti joby RustCI41643604662. Koordinator pote
konkretne zadal jeden novy actual3600s test. Byl zmrazen source i release-optimized
libtest oracle2df1ba0d...; profil je priznan a neprepisuje starsi debug FAILs,
071 idle nebo produkcni EXE. OwnPID30612 byl overen pred kolekci.

Tento beh take skoncil Overflow: elapsed38.0813763s, frame952, prijato1218560
bytes, nativni1280B/40ms/50slots nezmenene. Producer lateness pri faultu pouze
1.377ms; nelze tedy tvrdit, ze problem je pouze dohaneni zpozdeneho timeru.
Zachovano301warm+38capture rawrows, producercontext/stderr/encryptedfailure-state.
Neni3600s,50minrotation,postidle ani10MiBPASS. Ownprocess skoncil a dalsi
fullhour automaticky NESPUSTEN.

Read-only kontrola failed Recovery overila vsech1218560prijatychbytes jako
ulozene, confirmed1152000, capture_complete a puvodni synthetic binding.
Vsech66560nepotvrzenychPCMbytes overeno proti absolutnimu97-sample vzoru;
ciphertext pred/po nezmenen. Exact hashy a rozsah jsou v
controller-hour-4430f8e/partial-result-summary.json. Toto je ochrana dat po
preruseni, nikoliv nahrazeni hodinove akceptace.

Nasledovala pouze ohranicena30s/60s NoPorts diagnostika. Test-only probes
rozlisuji source admission, writer-received a completed-append bytes, vek
probihajiciho append a faze Recovery read/unprotect/protect/path/quota/
atomic-write/delete/progress-publication. Neukladaji PCM/token contents.
Dokonceny append neni vydavan za durable nebo confirmed cursor.
Tyto behy opet skoncily backlog/Overflow; nektere nedokoncily ani bounded
shutdown, vsechny failure-state a rawjournals zustavaji. Zadny neni PASS.

Konkretnimi pozorovanimi jsou append trvajici pres2s s nevycerpanou frontou
a jeden snapshot ve fazi path-metadata/quota1067.5209ms. Jejich presny
OS/scheduling/lock/filesystem podil ani definitivni odstraneni priciny nejsou
prokazane. Sampler pri zaniklem procesu nekdy vratil property Count chybu,
jindy explicitprocess-exit; v zadnem pripade nevydal kompletni capture report.

Zdrojova oprava admission fairness: async try_lock nesmi predbehnout jizcekajici
synchronous writer. Dve deterministicke engine skupiny tento invariant overuji.
Quota scan take znovupouziva enumerovana DirEntry metadata namisto dalsiho
per-entry syscallu. [Oficialni Rust dokumentace](https://doc.rust-lang.org/std/fs/struct.DirEntry.html#method.metadata)
potvrzuje non-following symlink semantics a zadny dalsi Windows syscall.
Root/path checks, reparse rejection, vsechny quota bounds a charging predchoziho
targetu/orphans se zachovavaji; actual C#/Rust Recovery7groups vcetnejunction/
ownership/data parity prochazeji. Tato optimalizace neni dukazem hodinoveho PASS.

Nove native110regular/fmt/Clippy a encryptedRecovery7groups PASS. Bounded
diagnostika stale FAILED/INCOMPLETE; zadne dalsi genericke keyboard/human okno,
quietprofile, physicalcapture/clipboard/auth/install/Azure nebo paidAI scope
se nepredpoklada. Cele dokonceni zustava neuzavrene; nutny je dokazatelne
stabilni current-controller wallclock a oddelene GUI/human/upgrade/privateAzure
akceptace, ne slepe opakovani hodin bez vysvetleni priciny.

### Internal owner/phase diagnostika bez instalace debuggeru

Po zadani koordinatora byla pridana test-only instrumentation skutecnych
streaming/legacy-state a recovery-root mutex guards. Zaznamenava acquire/wait/
release monotonic timestamp, konkretni operaci a vlastni Rust-thread key.
Monitor nepouziva sledovany mutex: pouze atomics a samostatny try_lock cache.
Omezene acquisition contexts jsou nejvyse64x16KiB; std::backtrace::force_capture
se vyvolava z owner threadu pri acquire a dalsi stack captures jsou po warmup
zmrazene. Neni to aktualni suspended kernel stack, OS thread ID, memory nebo
environment dump ani WinDbg sampling.

Diagnosticky release s debuginfo2/localPDB overil znamy fixture callsite pred
jedinym30s owner behom. Prvni known-marker assertion odhalila tail-call
optimalizaci pomocneho markeru; postcall black_box ji odstranil a exactsymbol
test nasledne prosel0.24s. Puvodni failedverification/exe zachovany. Novy
oracle46c294d7... a PDB26f7be9a... byly overene, zadne tooling/packageinstalls
nebo externi symbolserver/searchpath se nepouzily.

Owner probe skoncil FAILED/INCOMPLETE10.8118359s na Overflow. Jeho nove
konkretni dukazy: stable streaming-state epoch1136, Rustthread2,
operation send-audio-read, age1405.4594ms; stable recovery-root epoch1137,
stejny Rustthread2, NativeRecoveryJournal::read_audio, age1405.388ms.
Capturewriter Rustthread5 cekal v StreamingSession::append na state mutex.
Accepted345600/received281600/completed280320bytes; append-age1399.4276ms,
producermaxlateness1520.0907ms/currentlateness11.8371ms. Acquisitioncontext
ze sameho ownerthreadu propojuje read_audio->read_range->send_audio.
Generic symbol names mohou v optimized buildu sdilet kod; authoritative
owner je instrumented lockkind/operation/epoch, nikoliv odhad livecallstacku.
Raw owner/IO context a failedstate jsou v controller-owner-diagnostic-work.
Neni to hodina, performancePASS nebo prokazany OS/kernel latency cause.

Audit vsech session_path/audio_path callers odhalil redundantni filesystem
ancestor validation uz pri konstrukci path, nasledovanou stejnou plnou fresh
validaci v read_encrypted/write_encrypted/remove_audio. Konstrukce je nyni
lexical s puvodni strict GUID validaci. Skutecne I/O entry nadale overuji
plny aktualni target/ancestors/reparse/bounds; create/delete directory navic
maji explicitni guard hned pred filesystem operaci. Zadna validace se
necacheuje mezi operacemi a zadny cil/quota/budget se nesnizuje.

Aktualni308workspace/112native regular tests, fmt/all-target-all-feature
Clippy/release a7actual C#/Rust Recoveryparitygroups vcetne junction/escape/
ownership/data parity PASS. ExactcurrentCI a relevantni native/ikonove surface
validation se ulozi samostatne. Dalsi fullhour/quietprofile/device/CUI/human/
install/Azure scope se nepredpoklada; ownerdiagnostics neni jeho nahrada.

Aktualni6hidden lifecycle a7DPAPI/IPC/PE-icon groups take PASS.
Build-only setup36d15520a5528fd14ae33c11f1f802d6879113d2a4b14fe06f416a936b724e88
obsahuje release/stagedbedd72a93bd6f41bd5978d1b9e98bfa4785b85b80118675dd57b95aeb0824726.
Installer nebyl spusten/instalovan/publikovan a nebyl pouzit k nove physical
akceptaci. Presne CI tohoto source inkrementu zustava oddelenym dukazem.

### Bounded post-dedup60s: potrebne pozitivni owner/latency evidence

Koordinator konkretne zadal jeden60s current NoPorts wall-clock regression,
nikoliv dalsi hodinu nebo quietprofile. Pred jeho startem bylo overeno, ze
puvodni owner snapshot se publikoval jen pri failure a na success by chybely
pozadovane measuredhold/wait/occupancy doklady. Doplnene test-only atomicmaxima
a progress jsou vzorkovane kazdych10s na samostatnem boundedobserverthreadu;
ten nikdy neziskava engine/journal mutex ani nevytvari frontu audio dat.
Normalni i chybovy exit observerjoinne. Nepribyva stack nebo velkylog40mspacketu.

Nove owner snapshoty rozlisuji queueadmission/received/completedappend/
durablesaved/confirmed, maximalni observedqueuecapacity a measuredhold/wait
od vytvoreni prislusneho locku, bez vymyslenehoperformanceprahu.
Sourceproducer kontinuita1280B/40ms/50slots, boundedoverflow a stopbudgety
zustavaji beze zmeny. Dalsi unit test zablokuje actualjournal+engine lock a
overi ze snapshotmonitor netrva na jejichuvolneni.
Current309workspace/113native/fmt/Clippy PASS; matching source/CI/frozenhashes
se ulozi pred jedinym60srun. Po tomto doplneni nebyl spusten novyfullhour.

### Assigned post-dedup60s b00: FAILED, novy konkretni reader blocker

Sourceb00daa950827eb2d60e3186400d2c876bbf11cb9 a predchozie797 oba prosly
ALL5CI (41667807093/41665591227) pred jedinymzadanym60s testem.
Frozenoptimized+debuginfooracle78b1b4f9.../PDBfdcf7db0.../sourcearchive2e2504e2...
a local knownsymbols bylyoverene. Original1280B40ms50slots/stopbudgety se
nemenily a GUI/realaccount/network/physical/registry/Azure ports nevznikly.

Test skoncilOverflow24.6829066s: accepted789760, writerreceived725760,
completedappend724480bytes, observedqueuepeak50. Producercurrentlate2.9075ms,
maxearlierlate1367.9798ms.10ssnapshot zachytil1280queuedbytes/maxobserved4;
20ssnapshot25600queuedbytes/maxobserved50. Raw28capturerows a vsechny
owner/IO/failedstate/stderr doklady jsou uchovane.

Namereny actual blocker: send-audio-read/read_audio drzel engine i recovery-root
locks na Rustthread3. Maxenginehold2057.5865ms/maxwait2831.8491ms;
maxroothold2057.5499ms/maxwait0.0012ms. PriOverflow byla phase read-encrypted
active676.9262ms a ownerages priblizne677ms. Maxima jsou od vytvoreni locku,
nikoliv vymysleny acceptance threshold nebo doklad konkretniho OS/kernelwait.
Acquisition contexts zustavaji odlisene od aktualniho suspendedstacku.

Read-only failedRecovery nasledne overila vsech789760admittedbytes ulozenych,
confirmed512000/puvodnisyntheticbinding/capturecomplete. Verified302080pending
PCMbytes zahrnuji retainedoverlap; jsou sample-exact a ciphertextnezmeneny.
Ownerprocess38696gone, controltempremoved, encryptedfailure-state/rawdata
retained. Result-summary/failurehash/preservationproof jsou v
controller-post-path-60s-b00daa9. Zadny retry nebo novyfullhour.

Nesplneno1920000bytes/60s/normalpostidle a nelze tvrdit hour nebo finalmigrationPASS.
Priority dalsiho kroku je measuredreadercriticalsection/read-encryptedsubphase,
ne dalsi slepyhour nebo spekulativni thread/path refactor. Source/data/icon/
scopeCI dukazy zustavaji platne, ale currentwallclockstabilita stale chybi.
