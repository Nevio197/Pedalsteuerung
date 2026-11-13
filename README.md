# Pedalsteuerung · Made by Nevio

Pedalsteuerung macht das Philips ACC2320 zum Controller für Discord, OBS und Elgato Stream Deck. Drei Tasten, jeweils mit Tippen, Halten und Doppeltippen. Profile, Makros und Kalibrierung lassen sich in der App bearbeiten.

## Installieren

1. Lade die **DMG für Apple Silicon** aus den [GitHub Releases](https://github.com/Nevio197/Pedalsteuerung/releases/latest).
2. Öffne die DMG und zieh **Pedalsteuerung 2** in **Programme**.
3. Schließe das Philips-Pedal an und öffne die App. Prüfe die Tasten in der Vorschau.

Diese Version benötigt macOS 13 oder neuer und einen Mac mit Apple-Chip. Die App ist lokal signiert und noch nicht von Apple notarisiert. Falls macOS das Öffnen blockiert, starte die App einmal aus „Programme“ und erlaube sie anschließend unter **Systemeinstellungen → Datenschutz & Sicherheit → Dennoch öffnen**. Die [Anleitung](START-HIER.txt) erklärt die Tastenbelegung und die Einrichtung der Integrationen.

## Spielen und Updates

Aktiviere den Exklusivmodus, bevor du Steam oder das Spiel startest. Anwendungen, die das Philips-Pedal bereits geöffnet haben, können weiterhin Eingaben bekommen. Vorschau und Pause behalten den exklusiven Zugriff bei. Über **Beenden** im Menüleistensymbol gibt die App das Pedal frei.

Die App sucht beim Start und dann alle zwölf Stunden nach stabilen Releases. Unter **Einstellungen → Updates** lässt sich die Prüfung manuell auslösen. Ein verfügbares Update wird erst nach Auswahl installiert und die App neu gestartet. Die Update-Dateien sind mit dem öffentlichen Schlüssel der App überprüfbar.

## Feedback

[Bugs melden oder eine Idee vorschlagen](https://github.com/Nevio197/Pedalsteuerung/issues/new/choose). GitHub Issues sind öffentlich. Die App öffnet das passende Formular im Browser.

## Entwicklung

[Build und Release](QUELLCODE.md) · Rust und Tauri 2 mit einer lokalen Oberfläche aus HTML, CSS und JavaScript.

Made by Nevio.
