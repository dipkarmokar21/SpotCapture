#pragma once

static constexpr const char *kLayout = R"TCL(
package require Tk
wm title . "SpotCapture"
wm geometry . 1100x790
wm minsize . 920 700
wm protocol . WM_DELETE_WINDOW native::close
option add *Font {Sans 10}
option add *selectBackground #256a51
option add *selectForeground #f3faf7
option add *insertBackground #dce9e1
option add *background #111a18
option add *foreground #e7eee9
. configure -background #0b1210
ttk::style theme use clam
ttk::style configure TFrame -background #0b1210
ttk::style configure Card.TFrame -background #14201b
ttk::style configure TLabel -background #0b1210 -foreground #e7eee9
ttk::style configure Card.TLabel -background #14201b -foreground #e7eee9
ttk::style configure Muted.TLabel -background #0b1210 -foreground #99ada2
ttk::style configure Small.TLabel -font {Sans 9} -foreground #99ada2
ttk::style configure Title.TLabel -font {Sans 20 bold}
ttk::style configure Brand.TLabel -font {Sans 11 bold} -foreground #6fe5a6
ttk::style configure Heading.TLabel -font {Sans 12 bold} -background #14201b
ttk::style configure Caption.TLabel -font {Sans 9} -background #14201b -foreground #a5b9ae
ttk::style configure Metric.TLabel -font {Sans 24 bold} -background #14201b -foreground #6fe5a6
ttk::style configure TButton -padding {14 9} -background #273b30 -foreground #e7eee9 -borderwidth 0 -focusthickness 1 -focuscolor #6fe5a6
ttk::style map TButton -background {active #354f40 pressed #1d2e24 disabled #1c2721} -foreground {disabled #728477}
ttk::style configure Accent.TButton -background #68dea0 -foreground #0d2517 -font {Sans 10 bold}
ttk::style map Accent.TButton -background {active #8beab6 pressed #4dc588 disabled #283d30} -foreground {disabled #728477}
ttk::style configure TEntry -fieldbackground #0c1610 -foreground #eef5f0 -padding {10 9} -bordercolor #34483b -lightcolor #34483b -darkcolor #34483b -insertcolor #eef5f0
ttk::style map TEntry -bordercolor {focus #68dea0} -fieldbackground {disabled #17221b} -foreground {disabled #9aaca0}
ttk::style configure TRadiobutton -background #14201b -foreground #dce8df -padding {0 4}
ttk::style map TRadiobutton -background {active #14201b} -foreground {disabled #728477}
ttk::style configure Treeview -background #14201b -fieldbackground #14201b -foreground #e7eee9 -rowheight 34 -borderwidth 0 -font {Sans 10}
ttk::style configure Treeview.Heading -background #1b2c22 -foreground #9bb0a2 -padding {8 10} -font {Sans 9 bold} -borderwidth 0
ttk::style map Treeview -background {selected #234535} -foreground {selected #ebfff3}
ttk::style map Treeview.Heading -background {active #1b2c22}
ttk::style configure TScrollbar -background #304a3a -troughcolor #14201b -bordercolor #14201b -arrowcolor #9bb0a2 -width 11
ttk::style configure Horizontal.TProgressbar -background #68dea0 -troughcolor #24372b -borderwidth 0 -lightcolor #68dea0 -darkcolor #68dea0
ttk::style configure TSeparator -background #26392d

set trackInput ""
set clientId ""
set outputDir ""
set format mp3
set bitrate 320
set stateLabel "Ready"
set detailLabel "Paste a Spotify track link to begin."
set speedLabel "—"
set capturedLabel "0:00 captured"
set elapsedLabel "0:00 elapsed"
set queueCount "0 tracks"
set loginUrl ""
set progressPercent 0

ttk::frame .app -padding {24 16 24 14}
pack .app -fill both -expand 1
ttk::frame .app.brand
pack .app.brand -fill x
ttk::label .app.brand.name -text "●  SPOTCAPTURE" -style Brand.TLabel
pack .app.brand.name -side left
ttk::label .app.brand.tag -text "SYSTEM AUDIO CAPTURE" -style Small.TLabel
pack .app.brand.tag -side right
ttk::label .app.title -text "From stream to your library." -style Title.TLabel
pack .app.title -anchor w -pady {14 3}
ttk::label .app.subtitle -text "Spotify Desktop → PipeWire → Encoder   ·   Real-time silent capture at highest quality." -style Muted.TLabel
pack .app.subtitle -anchor w -pady {0 14}

ttk::frame .app.add
pack .app.add -fill x -pady {0 12}
ttk::entry .app.add.url -textvariable trackInput
pack .app.add.url -side left -fill x -expand 1 -ipady 2
ttk::button .app.add.button -text "+ Add track" -command {native::enqueue $trackInput}
pack .app.add.button -side right -padx {10 0}
ttk::button .app.add.download -text "⬇ Download" -style Accent.TButton -command {native::quickdownload $trackInput}
pack .app.add.download -side right -padx {10 0}
bind .app.add.url <Return> {native::quickdownload $trackInput}

ttk::frame .app.body
pack .app.body -fill both -expand 1
grid columnconfigure .app.body 0 -weight 1
grid columnconfigure .app.body 1 -minsize 314
grid rowconfigure .app.body 0 -weight 1
ttk::frame .app.body.left
grid .app.body.left -row 0 -column 0 -sticky nsew -padx {0 18}
ttk::frame .app.body.right -style Card.TFrame -padding 16
grid .app.body.right -row 0 -column 1 -sticky nsew

ttk::frame .app.body.left.queue -style Card.TFrame -padding {14 12 14 10}
pack .app.body.left.queue -fill both -expand 1
ttk::frame .app.body.left.queue.heading -style Card.TFrame
pack .app.body.left.queue.heading -fill x -pady {0 10}
ttk::label .app.body.left.queue.heading.title -text "Capture queue" -style Heading.TLabel
pack .app.body.left.queue.heading.title -side left
ttk::label .app.body.left.queue.heading.count -textvariable queueCount -style Caption.TLabel
pack .app.body.left.queue.heading.count -side right
ttk::frame .app.body.left.queue.list -style Card.TFrame
pack .app.body.left.queue.list -fill both -expand 1
ttk::treeview .app.body.left.queue.list.items -columns {track state} -show headings -height 3 -selectmode extended
.app.body.left.queue.list.items heading track -text "TRACK / SPOTIFY ID" -anchor w
.app.body.left.queue.list.items heading state -text "STATUS" -anchor w
.app.body.left.queue.list.items column track -width 250 -minwidth 150 -stretch 1
.app.body.left.queue.list.items column state -width 104 -minwidth 100 -stretch 0
ttk::scrollbar .app.body.left.queue.list.scroll -command {.app.body.left.queue.list.items yview}
.app.body.left.queue.list.items configure -yscrollcommand {.app.body.left.queue.list.scroll set}
pack .app.body.left.queue.list.scroll -side right -fill y
pack .app.body.left.queue.list.items -side left -fill both -expand 1
ttk::frame .app.body.left.queue.actions -style Card.TFrame
pack .app.body.left.queue.actions -fill x -pady {10 0}
ttk::button .app.body.left.queue.actions.remove -text "Remove selected" -command native::remove
pack .app.body.left.queue.actions.remove -side left
ttk::label .app.body.left.queue.actions.note -text "One track at a time" -style Caption.TLabel
pack .app.body.left.queue.actions.note -side right

ttk::frame .app.body.left.capture -style Card.TFrame -padding 14
pack .app.body.left.capture -fill x -pady {14 0}
ttk::frame .app.body.left.capture.top -style Card.TFrame
pack .app.body.left.capture.top -fill x
ttk::label .app.body.left.capture.top.state -textvariable stateLabel -style Heading.TLabel
pack .app.body.left.capture.top.state -side left
ttk::label .app.body.left.capture.top.speed -textvariable speedLabel -style Metric.TLabel
pack .app.body.left.capture.top.speed -side right
ttk::label .app.body.left.capture.detail -textvariable detailLabel -style Caption.TLabel -wraplength 475
pack .app.body.left.capture.detail -anchor w -pady {4 12}
ttk::progressbar .app.body.left.capture.bar -mode determinate -length 300 -variable progressPercent -maximum 100
pack .app.body.left.capture.bar -fill x
ttk::frame .app.body.left.capture.metrics -style Card.TFrame
pack .app.body.left.capture.metrics -fill x -pady {10 14}
ttk::label .app.body.left.capture.metrics.captured -textvariable capturedLabel -style Caption.TLabel
pack .app.body.left.capture.metrics.captured -side left
ttk::label .app.body.left.capture.metrics.elapsed -textvariable elapsedLabel -style Caption.TLabel
pack .app.body.left.capture.metrics.elapsed -side right
ttk::frame .app.body.left.capture.buttons -style Card.TFrame
pack .app.body.left.capture.buttons -fill x
ttk::button .app.body.left.capture.buttons.start -text "Start capture" -style Accent.TButton -command native::start
pack .app.body.left.capture.buttons.start -side left -fill x -expand 1
ttk::button .app.body.left.capture.buttons.cancel -text "Cancel" -command native::cancel -state disabled
pack .app.body.left.capture.buttons.cancel -side right -padx {10 0}

ttk::label .app.body.right.heading -text "Export settings" -style Heading.TLabel
pack .app.body.right.heading -anchor w -pady {0 4}
ttk::label .app.body.right.formatlabel -text "FORMAT" -style Caption.TLabel
pack .app.body.right.formatlabel -anchor w -pady {4 2}
ttk::frame .app.body.right.formats -style Card.TFrame
pack .app.body.right.formats -fill x -pady {0 7}
foreach {id title} {mp3 "MP3" flac "FLAC" wav "WAV"} {
    ttk::radiobutton .app.body.right.formats.$id -text $title -value $id -variable format
    pack .app.body.right.formats.$id -side left -padx {0 14}
}
ttk::label .app.body.right.bitratelabel -text "MP3 BITRATE" -style Caption.TLabel
pack .app.body.right.bitratelabel -anchor w -pady {4 2}
ttk::frame .app.body.right.bitrates -style Card.TFrame
pack .app.body.right.bitrates -fill x -pady {0 7}
foreach {id title} {128 "128k" 192 "192k" 256 "256k" 320 "320k"} {
    ttk::radiobutton .app.body.right.bitrates.b$id -text $title -value $id -variable bitrate
    pack .app.body.right.bitrates.b$id -side left -padx {0 14}
}

proc updateBitrateState {name1 name2 op} {
    global format
    if {$format == "mp3"} {
        foreach id {128 192 256 320} { .app.body.right.bitrates.b$id state !disabled }
    } else {
        foreach id {128 192 256 320} { .app.body.right.bitrates.b$id state disabled }
    }
}
trace add variable format write updateBitrateState
updateBitrateState format "" write
ttk::label .app.body.right.folderlabel -text "SAVE TO" -style Caption.TLabel
pack .app.body.right.folderlabel -anchor w -pady {4 0}
ttk::entry .app.body.right.folder -textvariable outputDir -width 25
pack .app.body.right.folder -fill x -pady {4 5}
ttk::button .app.body.right.browse -text "Choose folder…" -command native::browse
pack .app.body.right.browse -fill x
ttk::separator .app.body.right.sep -orient horizontal
pack .app.body.right.sep -fill x -pady 8
ttk::label .app.body.right.metaheading -text "Metadata (optional)" -style Heading.TLabel
pack .app.body.right.metaheading -anchor w -pady {0 5}
ttk::label .app.body.right.metanote -text "Connect Web API for artwork and\nextra tags. Not required for capture." -style Caption.TLabel -wraplength 280
pack .app.body.right.metanote -anchor w -pady {0 8}
ttk::label .app.body.right.clientlabel -text "SPOTIFY APP CLIENT ID" -style Caption.TLabel
pack .app.body.right.clientlabel -anchor w
ttk::entry .app.body.right.client -textvariable clientId -width 25
pack .app.body.right.client -fill x -pady {4 5}
ttk::button .app.body.right.login -text "Connect metadata" -command native::login
pack .app.body.right.login -fill x
ttk::button .app.body.right.openlogin -text "Open sign-in link" -command native::openlogin

ttk::frame .app.loghead
pack .app.loghead -fill x -pady {12 5}
ttk::label .app.loghead.title -text "ACTIVITY" -style Small.TLabel
pack .app.loghead.title -side left
text .app.log -height 3 -wrap word -background #0e1812 -foreground #9ab6a2 -relief flat -borderwidth 0 -padx 12 -pady 8 -font {Monospace 9} -state disabled -highlightthickness 1 -highlightbackground #213629
pack .app.log -fill x
ttk::label .app.footer -text "Real-time capture from Spotify Desktop via PipeWire  ·  Speaker is silenced during recording." -style Small.TLabel
pack .app.footer -anchor w -pady {10 0}
focus .app.add.url

proc uiLog {message} {
    .app.log configure -state normal
    .app.log insert end "$message\n"
    if {[lindex [split [.app.log index end] .] 0] > 500} {
        .app.log delete 1.0 100.0
    }
    .app.log see end
    .app.log configure -state disabled
}
proc uiBusy {busy} {
    set enabled [expr {$busy ? "disabled" : "normal"}]
    .app.body.left.capture.buttons.start configure -state $enabled
    .app.body.right.login configure -state $enabled
    .app.body.right.client configure -state $enabled
    .app.body.right.folder configure -state $enabled
    .app.body.right.browse configure -state $enabled
    .app.add.button configure -state $enabled
    .app.add.download configure -state $enabled
    foreach id {mp3 flac wav} {.app.body.right.formats.$id configure -state $enabled}
    foreach id {128 192 256 320} {.app.body.right.bitrates.b$id configure -state $enabled}
    .app.body.left.capture.buttons.cancel configure -state [expr {$busy ? "normal" : "disabled"}]
    if {$busy} {.app.body.left.capture.bar start 20} else {.app.body.left.capture.bar stop}
}
proc uiRow {id label status} {
    set tree .app.body.left.queue.list.items
    if {[$tree exists $id]} {
        $tree item $id -values [list $label $status]
    } else {
        $tree insert {} end -id $id -values [list $label $status]
    }
}
proc uiLoginLink {url} {
    set ::loginUrl $url
    pack .app.body.right.openlogin -fill x -after .app.body.right.login -pady {6 0}
}
)TCL";
