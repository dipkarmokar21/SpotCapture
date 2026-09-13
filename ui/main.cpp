#include <tcl.h>
#include <tk.h>

#include <algorithm>
#include <cerrno>
#include <chrono>
#include <cctype>
#include <cmath>
#include <csignal>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <fcntl.h>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <map>
#include <sstream>
#include <string>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>
#include <vector>

#include "layout.hpp"

using Clock = std::chrono::steady_clock;
namespace fs = std::filesystem;

// Only the flat JSON event objects emitted by the engine are needed here.
// Parse strings (including escapes) rather than evaluating child output as Tcl.
class JsonObject {
public:
    std::map<std::string, std::string> values;
    explicit JsonObject(const std::string &source) : source_(source) {
        try {
            whitespace(); expect('{'); whitespace();
            if (peek() == '}') { ++at_; valid_ = true; return; }
            while (true) {
                const auto key = string(); whitespace(); expect(':'); whitespace();
                std::string value;
                if (peek() == '"') value = string();
                else {
                    const auto start = at_;
                    while (at_ < source_.size() && source_[at_] != ',' && source_[at_] != '}') ++at_;
                    value = source_.substr(start, at_ - start);
                }
                values[key] = value; whitespace();
                if (peek() == '}') { ++at_; break; }
                expect(','); whitespace();
            }
            whitespace(); valid_ = at_ == source_.size();
        } catch (...) { valid_ = false; }
    }
    bool valid() const { return valid_; }
    std::string get(const std::string &key) const {
        const auto it = values.find(key); return it == values.end() ? "" : it->second;
    }
    double number(const std::string &key) const {
        try { const auto n = std::stod(get(key)); return std::isfinite(n) ? n : 0.0; }
        catch (...) { return 0.0; }
    }
private:
    const std::string &source_;
    size_t at_ = 0;
    bool valid_ = false;
    char peek() const { if (at_ >= source_.size()) throw 1; return source_[at_]; }
    void whitespace() { while (at_ < source_.size() && std::isspace(static_cast<unsigned char>(source_[at_]))) ++at_; }
    void expect(char c) { if (peek() != c) throw 1; ++at_; }
    unsigned hex4() {
        unsigned n = 0;
        for (int i = 0; i < 4; ++i) {
            const char c = peek(); ++at_; n <<= 4;
            if (c >= '0' && c <= '9') n += c - '0';
            else if (c >= 'a' && c <= 'f') n += c - 'a' + 10;
            else if (c >= 'A' && c <= 'F') n += c - 'A' + 10;
            else throw 1;
        }
        return n;
    }
    static void utf8(std::string &out, unsigned cp) {
        if (cp < 0x80) out += static_cast<char>(cp);
        else if (cp < 0x800) { out += static_cast<char>(0xc0 | (cp >> 6)); out += static_cast<char>(0x80 | (cp & 63)); }
        else if (cp < 0x10000) { out += static_cast<char>(0xe0 | (cp >> 12)); out += static_cast<char>(0x80 | ((cp >> 6) & 63)); out += static_cast<char>(0x80 | (cp & 63)); }
        else { out += static_cast<char>(0xf0 | (cp >> 18)); out += static_cast<char>(0x80 | ((cp >> 12) & 63)); out += static_cast<char>(0x80 | ((cp >> 6) & 63)); out += static_cast<char>(0x80 | (cp & 63)); }
    }
    std::string string() {
        expect('"'); std::string out;
        while (true) {
            const char c = peek(); ++at_;
            if (c == '"') return out;
            if (static_cast<unsigned char>(c) < 0x20) throw 1;
            if (c != '\\') { out += c; continue; }
            const char e = peek(); ++at_;
            switch (e) {
                case '"': case '\\': case '/': out += e; break;
                case 'b': out += '\b'; break; case 'f': out += '\f'; break;
                case 'n': out += '\n'; break; case 'r': out += '\r'; break; case 't': out += '\t'; break;
                case 'u': {
                    unsigned cp = hex4();
                    if (cp >= 0xd800 && cp <= 0xdbff) {
                        expect('\\'); expect('u'); const unsigned low = hex4();
                        if (low < 0xdc00 || low > 0xdfff) throw 1;
                        cp = 0x10000 + ((cp - 0xd800) << 10) + low - 0xdc00;
                    } else if (cp >= 0xdc00 && cp <= 0xdfff) throw 1;
                    utf8(out, cp); break;
                }
                default: throw 1;
            }
        }
    }
};

static std::string trim(std::string s) {
    auto whitespace = [](unsigned char c) { return std::isspace(c); };
    s.erase(s.begin(), std::find_if_not(s.begin(), s.end(), whitespace));
    s.erase(std::find_if_not(s.rbegin(), s.rend(), whitespace).base(), s.end()); return s;
}

static std::string trackId(const std::string &input) {
    std::string id;
    if (input.rfind("spotify:track:", 0) == 0) id = input.substr(14);
    else if (input.rfind("https://open.spotify.com/", 0) == 0) {
        auto path = input.substr(25);
        if (path.rfind("intl-", 0) == 0) {
            const auto slash = path.find('/'); if (slash == std::string::npos) return "";
            path = path.substr(slash + 1);
        }
        if (path.rfind("track/", 0) != 0) return "";
        id = path.substr(6); id = id.substr(0, id.find_first_of("?#/"));
    } else return "";
    if (id.size() != 22 || !std::all_of(id.begin(), id.end(), [](unsigned char c) { return std::isalnum(c) && c < 128; })) return "";
    return id;
}

static std::string duration(double seconds) {
    const auto n = static_cast<long long>(std::max(0.0, seconds));
    std::ostringstream out; out << (n / 60) << ':' << std::setfill('0') << std::setw(2) << (n % 60); return out.str();
}

struct Track { std::string id, input, label, state = "Queued"; };

static size_t prepareManualStart(std::vector<Track> &queue) {
    size_t retried = 0;
    for (auto &track : queue) {
        if (track.state == "Failed" || track.state == "Cancelled") {
            track.state = "Queued";
            ++retried;
        }
    }
    return retried;
}

static std::vector<Track>::iterator nextQueuedTrack(std::vector<Track> &queue) {
    return std::find_if(queue.begin(), queue.end(), [](const Track &track) { return track.state == "Queued"; });
}

class App {
public:
    Tcl_Interp *interp;
    fs::path engine;
    explicit App(Tcl_Interp *i, fs::path executable) : interp(i), engine(std::move(executable)) {}
    void set(const char *key, const std::string &value) { Tcl_SetVar2Ex(interp, key, nullptr, Tcl_NewStringObj(value.data(), static_cast<int>(value.size())), TCL_GLOBAL_ONLY); }
    std::string get(const char *key) { const char *value = Tcl_GetVar(interp, key, TCL_GLOBAL_ONLY); return value ? value : ""; }
    int call(std::initializer_list<std::string> args) {
        std::vector<Tcl_Obj *> objects;
        for (const auto &arg : args) { auto *o = Tcl_NewStringObj(arg.data(), static_cast<int>(arg.size())); Tcl_IncrRefCount(o); objects.push_back(o); }
        const int code = Tcl_EvalObjv(interp, static_cast<int>(objects.size()), objects.data(), TCL_EVAL_GLOBAL);
        if (code != TCL_OK) std::cerr << "UI: " << Tcl_GetStringResult(interp) << '\n';
        for (auto *o : objects) Tcl_DecrRefCount(o);
        return code;
    }
    void log(const std::string &message) { call({"uiLog", message}); }
    void initialize() {
        const char *home = std::getenv("HOME");
        const char *xdg = std::getenv("XDG_CONFIG_HOME");
        configDir_ = (xdg && *xdg) ? fs::path(xdg) / "spotcapture" : fs::path(home ? home : ".") / ".config/spotcapture";
        set("outputDir", (fs::path(home ? home : ".") / "Music/SpotCapture").string());
        std::ifstream prefs(configDir_ / "ui.conf"); std::string key, value;
        while (prefs >> key >> std::quoted(value)) {
            if (key == "clientId" || key == "outputDir"
                || (key == "format" && (value == "mp3" || value == "flac" || value == "wav"))
                || (key == "bitrate" && (value == "128" || value == "192" || value == "256" || value == "320")))
                set(key.c_str(), value);
        }
        if (::access(engine.c_str(), X_OK) != 0) log("Capture engine is not built yet: " + engine.string());
        else log("Ready. Paste a Spotify track link, then click Download.");
        Tcl_CreateTimerHandler(100, &App::timer, this);
    }
    void save() {
        std::error_code ec; fs::create_directories(configDir_, ec);
        if (ec) { log("Could not save preferences: " + ec.message()); return; }
        std::ofstream prefs(configDir_ / "ui.conf");
        for (const auto &key : {"clientId", "outputDir", "format", "bitrate"}) prefs << key << ' ' << std::quoted(get(key)) << '\n';
    }
    
    std::vector<Track> extractTracks(const std::string &url) {
        std::vector<Track> tracks;
        log("Extracting tracks from album/playlist...");
        std::string cmd = engine.string() + " extract \"" + url + "\"";
        FILE* pipe = popen(cmd.c_str(), "r");
        if (!pipe) {
            log("Failed to run extraction command.");
            return tracks;
        }
        char buffer[1024];
        while (fgets(buffer, sizeof(buffer), pipe) != nullptr) {
            std::string line(buffer);
            line = trim(line);
            if (line.rfind("spotify:track:", 0) == 0) {
                auto pipe_pos = line.find('|');
                std::string t_id, t_label;
                if (pipe_pos != std::string::npos) {
                    t_id = line.substr(14, pipe_pos - 14);
                    t_label = line.substr(pipe_pos + 1);
                } else {
                    t_id = line.substr(14);
                    t_label = t_id;
                }
                Track track; track.id = "track" + std::to_string(++counter_); track.input = "spotify:track:" + t_id; track.label = t_label;
                tracks.push_back(track);
            }
        }
        pclose(pipe);
        if (tracks.empty()) log("No tracks found in the provided link.");
        else log("Extracted " + std::to_string(tracks.size()) + " tracks.");
        return tracks;
    }
    
    int command(const std::string &name, int objc, Tcl_Obj *const objv[]) {
        if (name == "enqueue") {
            if (objc != 2) return TCL_ERROR;
            const std::string input = trim(Tcl_GetString(objv[1])); const auto id = trackId(input);
            if (id.empty()) {
                if (input.find("/album/") != std::string::npos || input.find("/playlist/") != std::string::npos) {
                    auto tracks = extractTracks(input);
                    if (tracks.empty()) return TCL_OK;
                    for (const auto& track : tracks) {
                        queue_.push_back(track); row(queue_.back());
                    }
                    count(); set("trackInput", "");
                    set("detailLabel", "Tracks added. Choose export settings, then start capture.");
                    return TCL_OK;
                } else {
                    log("Use a Spotify track, album, or playlist URL."); return TCL_OK; 
                }
            }
            Track track; track.id = "track" + std::to_string(++counter_); track.input = "spotify:track:" + id; track.label = id;
            queue_.push_back(track); row(queue_.back()); count(); set("trackInput", "");
            set("detailLabel", "Track added. Choose export settings, then start capture.");
        } else if (name == "quickdownload") {
            // Single-click download: enqueue + auto-start
            if (child_ > 0) { log("A capture is already running."); return TCL_OK; }
            if (objc != 2) return TCL_ERROR;
            const std::string input = trim(Tcl_GetString(objv[1])); const auto id = trackId(input);
            if (trim(get("outputDir")).empty()) { log("Choose an output folder first."); return TCL_OK; }
            if (id.empty()) {
                if (input.find("/album/") != std::string::npos || input.find("/playlist/") != std::string::npos) {
                    auto tracks = extractTracks(input);
                    if (tracks.empty()) return TCL_OK;
                    for (const auto& track : tracks) {
                        queue_.push_back(track); row(queue_.back());
                    }
                    count(); set("trackInput", "");
                    save(); batch_ = true; next();
                    return TCL_OK;
                } else {
                    log("Use a Spotify track, album, or playlist URL."); return TCL_OK; 
                }
            }
            Track track; track.id = "track" + std::to_string(++counter_); track.input = "spotify:track:" + id; track.label = id;
            queue_.push_back(track); row(queue_.back()); count(); set("trackInput", "");
            save(); batch_ = true; next();
        } else if (name == "start") {
            if (child_ > 0) return TCL_OK;
            if (trim(get("outputDir")).empty()) { log("Choose an output folder first."); return TCL_OK; }
            const auto retried = prepareManualStart(queue_);
            if (retried > 0) {
                for (const auto &track : queue_) row(track);
                log("Retrying " + std::to_string(retried) + (retried == 1 ? " failed or cancelled track." : " failed or cancelled tracks."));
            }
            save(); batch_ = true; next();
        } else if (name == "login") {
            if (child_ > 0) return TCL_OK;
            const auto client = trim(get("clientId"));
            if (client.empty()) { log("Enter your public Spotify app Client ID to connect metadata."); return TCL_OK; }
            save(); batch_ = false; active_.clear();
            spawn({"login", "--client-id", client});
        } else if (name == "cancel") cancel();
        else if (name == "close") {
            save(); closing_ = true;
            if (child_ > 0) { log("Closing: waiting for the capture to stop safely…"); cancel(); }
            else call({"destroy", "."});
        } else if (name == "remove") {
            call({".app.body.left.queue.list.items", "selection"});
            const std::string ids = Tcl_GetStringResult(interp);
            if (ids.empty()) return TCL_OK;
            std::stringstream ss(ids);
            std::string id;
            while (ss >> id) {
                if (id == active_) { log("Skipping the active capture, cancel it first."); continue; }
                queue_.erase(std::remove_if(queue_.begin(), queue_.end(), [&](const Track &t) { return t.id == id; }), queue_.end());
                call({".app.body.left.queue.list.items", "delete", id}); 
            }
            count();
        } else if (name == "browse") {
            call({"tk_chooseDirectory", "-title", "Save captured audio to", "-initialdir", get("outputDir"), "-mustexist", "0"});
            const std::string result = Tcl_GetStringResult(interp); if (!result.empty()) set("outputDir", result);
        } else if (name == "openlogin") openLogin();
        return TCL_OK;
    }
private:
    fs::path configDir_;
    std::vector<Track> queue_;
    std::vector<pid_t> browserChildren_;
    std::string active_, stdoutBuffer_, stderrBuffer_, donePath_, doneMessage_;
    pid_t child_ = -1;
    int stdoutFd_ = -1, stderrFd_ = -1;
    unsigned counter_ = 0;
    bool batch_ = false, cancelled_ = false, closing_ = false, receivedDone_ = false, receivedError_ = false;
    bool isLogin_ = false;
    int stopStage_ = 0;
    Clock::time_point started_, stopped_;
    void count() { set("queueCount", std::to_string(queue_.size()) + (queue_.size() == 1 ? " track" : " tracks")); }
    void row(const Track &track) { call({"uiRow", track.id, track.label, track.state}); }
    void mark(const std::string &state) { for (auto &track : queue_) if (track.id == active_) { track.state = state; row(track); } }
    void busy(bool value) { call({"uiBusy", value ? "1" : "0"}); }
    void next() {
        if (!batch_ || closing_) return;
        const auto it = nextQueuedTrack(queue_);
        if (it == queue_.end()) {
            batch_ = false; busy(false); active_.clear();
            set("stateLabel", "Queue finished"); set("detailLabel", "All queued tracks have been processed. Review each track's status above."); return;
        }
        active_ = it->id;
        std::vector<std::string> args {"download", it->input, "--format", get("format"), "--bitrate", get("bitrate"), "--output-dir", get("outputDir")};
        const auto client = trim(get("clientId")); if (!client.empty()) { args.push_back("--client-id"); args.push_back(client); }
        mark("Starting");
        if (!spawn(args)) { mark("Failed"); batch_ = false; active_.clear(); }
    }
    bool spawn(const std::vector<std::string> &args) {
        if (::access(engine.c_str(), X_OK) != 0) {
            log("Engine unavailable. Build the Rust engine first: " + engine.string());
            set("stateLabel", "Engine missing"); set("detailLabel", "Build the capture engine before starting."); return false;
        }
        int out[2], err[2];
        if (::pipe2(out, O_CLOEXEC) != 0) { log("Could not create output pipe: " + std::string(std::strerror(errno))); return false; }
        if (::pipe2(err, O_CLOEXEC) != 0) { ::close(out[0]); ::close(out[1]); log("Could not create error pipe."); return false; }
        std::vector<std::string> fullArgs {engine.string()}; fullArgs.insert(fullArgs.end(), args.begin(), args.end());
        std::vector<char *> argv; for (auto &arg : fullArgs) argv.push_back(arg.data()); argv.push_back(nullptr);
        isLogin_ = !args.empty() && args[0] == "login";
        const pid_t pid = ::fork();
        if (pid == 0) {
            ::setpgid(0, 0);
            ::dup2(out[1], STDOUT_FILENO); ::dup2(err[1], STDERR_FILENO);
            const int input = ::open("/dev/null", O_RDONLY); if (input >= 0) { ::dup2(input, STDIN_FILENO); ::close(input); }
            ::close(out[0]); ::close(out[1]); ::close(err[0]); ::close(err[1]);
            ::execv(engine.c_str(), argv.data());
            const char message[] = "Failed to launch capture engine.\n";
            const auto written = ::write(STDERR_FILENO, message, sizeof(message) - 1); (void)written; _exit(127);
        }
        ::close(out[1]); ::close(err[1]);
        if (pid < 0) { ::close(out[0]); ::close(err[0]); log("Could not start capture process."); return false; }
        ::setpgid(pid, pid);
        child_ = pid; stdoutFd_ = out[0]; stderrFd_ = err[0];
        ::fcntl(stdoutFd_, F_SETFL, ::fcntl(stdoutFd_, F_GETFL) | O_NONBLOCK);
        ::fcntl(stderrFd_, F_SETFL, ::fcntl(stderrFd_, F_GETFL) | O_NONBLOCK);
        stdoutBuffer_.clear(); stderrBuffer_.clear(); donePath_.clear(); doneMessage_.clear();
        cancelled_ = false; receivedDone_ = false; receivedError_ = false; stopStage_ = 0; started_ = Clock::now();
        call({"pack", "forget", ".app.body.right.openlogin"}); set("loginUrl", "");
        busy(true); set("stateLabel", isLogin_ ? "Connecting metadata" : "Starting capture");
        set("detailLabel", isLogin_ ? "Follow the sign-in link in your browser." : "Preparing PipeWire capture…");
        set("speedLabel", "1.0×"); set("capturedLabel", "0:00 captured"); set("elapsedLabel", "0:00 elapsed");
        log(isLogin_ ? "Starting Spotify metadata sign-in (optional artwork and tags)…" : "Starting system audio capture…");
        return true;
    }
    void cancel() {
        batch_ = false;
        if (child_ <= 0 || cancelled_) return;
        cancelled_ = true; stopped_ = Clock::now(); stopStage_ = 1;
        ::kill(child_, SIGINT); set("stateLabel", "Stopping"); set("detailLabel", "Waiting for the capture to stop and restore audio…");
        log("Cancellation requested. Waiting for cleanup.");
    }
    void event(const std::string &line) {
        if (line.empty()) return;
        JsonObject event(line);
        if (!event.valid()) { log(line); return; }
        auto type = event.get("type"); if (type.empty()) type = event.get("event");
        const auto message = event.get("message");
        if (type == "progress") {
            set("speedLabel", "1.0×"); set("capturedLabel", duration(event.number("captured_seconds")) + " captured");
            set("elapsedLabel", duration(event.number("elapsed_seconds")) + " elapsed");
            if (!cancelled_) { set("stateLabel", "Recording"); mark("Recording"); }
            if (!message.empty()) set("detailLabel", message);
            double captured = event.number("captured_seconds");
            double total = event.number("duration_seconds");
            if (total > 0.0) {
                int percent = static_cast<int>((captured / total) * 100.0);
                if (percent > 100) percent = 100;
                set("progressPercent", std::to_string(percent));
            } else {
                set("progressPercent", "0");
            }
        } else if (type == "done") {
            receivedDone_ = true; donePath_ = event.get("path"); doneMessage_ = message;
            set("detailLabel", isLogin_ ? "Finishing sign-in…" : "Finalizing capture…");
        } else if (type == "error") {
            receivedError_ = true; log(message.empty() ? line : message); set("detailLabel", message);
        } else if (type == "login_url") {
            const auto url = event.get("url");
            if (!url.empty()) { call({"uiLoginLink", url}); log("Use 'Open sign-in link' to authorize Spotify in your browser."); }
            if (!message.empty()) log(message);
        } else if (!message.empty()) {
            log(message); if (!cancelled_) set("detailLabel", message);
        } else log(line);
    }
    void drain(int &fd, std::string &buffer, bool stderrStream) {
        if (fd < 0) return;
        char data[8192]; size_t total = 0;
        while (total < 262144) {
            const auto n = ::read(fd, data, sizeof(data));
            if (n == 0) { ::close(fd); fd = -1; break; }
            if (n < 0) { if (errno == EINTR) continue; if (errno != EAGAIN && errno != EWOULDBLOCK) { ::close(fd); fd = -1; } break; }
            total += static_cast<size_t>(n); buffer.append(data, static_cast<size_t>(n));
            size_t newline;
            while ((newline = buffer.find('\n')) != std::string::npos) {
                auto line = buffer.substr(0, newline); buffer.erase(0, newline + 1);
                if (!line.empty() && line.back() == '\r') line.pop_back();
                if (stderrStream) { if (!line.empty()) log(line); } else event(line);
            }
            if (buffer.size() > 65536) { log(buffer.substr(0, 65536)); buffer.clear(); }
        }
        if (fd < 0 && !buffer.empty()) { if (stderrStream) log(buffer); else event(buffer); buffer.clear(); }
    }
    void complete(int status) {
        drain(stdoutFd_, stdoutBuffer_, false); drain(stderrFd_, stderrBuffer_, true);
        if (stdoutFd_ >= 0) { ::close(stdoutFd_); stdoutFd_ = -1; }
        if (stderrFd_ >= 0) { ::close(stderrFd_); stderrFd_ = -1; }
        const bool success = WIFEXITED(status) && WEXITSTATUS(status) == 0 && !receivedError_;
        child_ = -1; busy(false);
        if (cancelled_) {
            mark("Cancelled"); set("stateLabel", isLogin_ ? "Metadata sign-in cancelled" : "Capture cancelled");
            set("detailLabel", isLogin_ ? "Metadata sign-in stopped. Use Connect metadata to try again." : "Capture stopped. Press Start capture to retry.");
            log(isLogin_ ? "Spotify sign-in process stopped." : "Capture process stopped.");
        } else if (success && isLogin_) {
            set("stateLabel", "Metadata connected");
            set("detailLabel", "Artwork and tags connected. Download tracks with full metadata.");
            log("Spotify metadata connection completed.");
            if (!doneMessage_.empty()) log(doneMessage_);
            call({"pack", "forget", ".app.body.right.openlogin"}); set("loginUrl", "");
        } else if (success && receivedDone_) {
            mark("Saved"); set("stateLabel", "Saved"); set("detailLabel", donePath_);
            log(donePath_.empty() ? "Capture completed." : "Saved: " + donePath_);
        } else {
            mark("Failed"); set("stateLabel", isLogin_ ? "Metadata sign-in failed" : "Capture failed"); batch_ = false;
            std::string reason = isLogin_ ? "Sign-in engine exited " : "Capture engine exited ";
            reason += WIFEXITED(status) ? "with status " + std::to_string(WEXITSTATUS(status)) : "after signal " + std::to_string(WTERMSIG(status));
            if (success && !receivedDone_) reason += " without confirming a saved file";
            log(reason + ".");
            set("detailLabel", isLogin_ ? "See activity for details. Use Connect metadata to retry." : "See activity for details. Press Start capture to retry.");
        }
        active_.clear();
        if (closing_) call({"destroy", "."}); else if (batch_) next();
    }
    void openLogin() {
        const std::string url = get("loginUrl");
        if (url.rfind("https://accounts.spotify.com/", 0) != 0) { log("No valid Spotify sign-in link is available yet."); return; }
        const pid_t pid = ::fork();
        if (pid == 0) {
            const int null = ::open("/dev/null", O_RDWR);
            if (null >= 0) { ::dup2(null, 0); ::dup2(null, 1); ::dup2(null, 2); if (null > 2) ::close(null); }
            ::execlp("xdg-open", "xdg-open", url.c_str(), static_cast<char *>(nullptr)); _exit(127);
        }
        if (pid > 0) browserChildren_.push_back(pid); else log("Could not start your web browser.");
    }
    void poll() {
        browserChildren_.erase(std::remove_if(browserChildren_.begin(), browserChildren_.end(), [&](pid_t pid) {
            int status = 0; const auto result = ::waitpid(pid, &status, WNOHANG);
            if (result == pid && (!WIFEXITED(status) || WEXITSTATUS(status) != 0)) log("Could not open the browser. Sign in using the URL in the activity log.");
            return result == pid || (result < 0 && errno == ECHILD);
        }), browserChildren_.end());
        if (child_ > 0) {
            drain(stdoutFd_, stdoutBuffer_, false); drain(stderrFd_, stderrBuffer_, true);
            set("elapsedLabel", duration(std::chrono::duration<double>(Clock::now() - started_).count()) + " elapsed");
            if (cancelled_) {
                const auto seconds = std::chrono::duration<double>(Clock::now() - stopped_).count();
                if (stopStage_ == 1 && seconds > 5) { log("Engine did not stop within 5 seconds; terminating its process group."); ::kill(-child_, SIGTERM); stopStage_ = 2; }
                else if (stopStage_ == 2 && seconds > 7) { log("Engine still unresponsive; forcing its process group to stop."); ::kill(-child_, SIGKILL); stopStage_ = 3; }
            }
            int status = 0; const auto result = ::waitpid(child_, &status, WNOHANG);
            if (result == child_) complete(status);
            else if (result < 0 && errno == ECHILD) { receivedError_ = true; complete(1 << 8); }
        }
        if (Tk_GetNumMainWindows() > 0) Tcl_CreateTimerHandler(100, &App::timer, this);
    }
    static void timer(ClientData data) { static_cast<App *>(data)->poll(); }
};

static int dispatch(ClientData data, Tcl_Interp *, int objc, Tcl_Obj *const objv[]) {
    auto *app = static_cast<App *>(data);
    const std::string full = Tcl_GetString(objv[0]); const auto name = full.substr(full.find("::") + 2);
    return app->command(name, objc, objv);
}

static int check() {
    auto require = [](bool ok, const char *message) { if (!ok) { std::cerr << "FAIL: " << message << '\n'; std::exit(1); } };
    require(Tcl_CommandComplete(kLayout), "embedded Tcl layout is incomplete");
    require(trackId("https://open.spotify.com/track/4uLU6hMCjMI75M1A2tKUQC?si=test") == "4uLU6hMCjMI75M1A2tKUQC", "track URL");
    require(trackId("https://open.spotify.com/intl-bn/track/4uLU6hMCjMI75M1A2tKUQC") == "4uLU6hMCjMI75M1A2tKUQC", "localized track URL");
    require(trackId("spotify:track:4uLU6hMCjMI75M1A2tKUQC") == "4uLU6hMCjMI75M1A2tKUQC", "track URI");
    require(trackId("https://attacker.invalid/track/4uLU6hMCjMI75M1A2tKUQC").empty(), "non-Spotify URL rejection");
    require(trackId("spotify:track:$(touch BAD)").empty(), "invalid ID rejection");
    const std::string json = R"({"type":"progress","captured_seconds":240.5,"speed":8.1,"message":"line\n\"quote\"","path":"\u09ac\u09be\u0982\u09b2\u09be \ud83c\udfb5"})";
    const JsonObject event(json);
    require(event.valid() && event.get("type") == "progress" && event.number("speed") == 8.1, "engine event parsing");
    require(event.get("message") == "line\n\"quote\"", "JSON escape decoding");
    require(event.get("path") == "বাংলা 🎵", "Unicode metadata decoding");
    const std::string malformed = "{\"type\":\"progress\"";
    require(!JsonObject(malformed).valid(), "truncated event rejection");
    require(duration(240.5) == "4:00", "duration formatting");
    std::vector<Track> queue {
        {"saved", "spotify:track:saved", "Saved song", "Saved"},
        {"failed", "spotify:track:failed", "Failed song", "Failed"},
        {"cancelled", "spotify:track:cancelled", "Cancelled song", "Cancelled"},
        {"pending", "spotify:track:pending", "Pending song", "Queued"}
    };
    require(nextQueuedTrack(queue)->id == "pending", "automatic advancement skips failed and cancelled tracks");
    require(prepareManualStart(queue) == 2, "manual start retries failed and cancelled tracks");
    require(queue[0].state == "Saved" && queue.size() == 4, "manual retry preserves saved rows without duplicates");
    require(prepareManualStart(queue) == 0, "queued retries are not requeued twice");
    auto current = nextQueuedTrack(queue);
    require(current != queue.end() && current->id == "failed", "failed track is eligible on manual retry");
    current->state = "Starting";
    current->state = "Failed";
    current = nextQueuedTrack(queue);
    require(current != queue.end() && current->id == "cancelled", "a repeated failure cannot automatically retry itself");
    current->state = "Saved";
    current = nextQueuedTrack(queue);
    require(current != queue.end() && current->id == "pending", "automatic advancement continues remaining queued tracks");
    current->state = "Saved";
    require(nextQueuedTrack(queue) == queue.end(), "automatic queue finishes with a failed row still present");
    require(prepareManualStart(queue) == 1, "another manual start permits one new failed-track attempt");
    current = nextQueuedTrack(queue);
    require(current != queue.end() && current->id == "failed", "manual retry targets remaining failure");
    current->state = "Saved";
    require(prepareManualStart(queue) == 0 && nextQueuedTrack(queue) == queue.end(), "saved queue does not recapture songs");
    std::cout << "SpotCapture UI checks passed (retry flow, event parsing, track validation, Tcl completeness).\n";
    return 0;
}

int main(int argc, char **argv) {
    if (argc > 1 && std::string(argv[1]) == "--help") {
        std::cout << "SpotCapture — system audio capture interface\n\nUsage: spotcapture-ui [--engine PATH] [--check | --smoke-test]\n\nCaptures Spotify Desktop audio via PipeWire.\nRequires a graphical desktop with Tcl/Tk 8.6.\n--smoke-test opens the UI for 1.5 seconds without starting capture.\n"; return 0;
    }
    if (argc > 1 && std::string(argv[1]) == "--check") return check();
    std::error_code ec; auto self = fs::read_symlink("/proc/self/exe", ec);
    fs::path engine = (ec ? fs::absolute(argv[0]) : self).parent_path() / "spotcapture";
    const bool smokeTest = argc == 2 && std::string(argv[1]) == "--smoke-test";
    if (argc == 3 && std::string(argv[1]) == "--engine") engine = fs::absolute(argv[2]);
    else if (smokeTest) {}
    else if (argc != 1) { std::cerr << "Unknown arguments. Use --help.\n"; return 2; }
    Tcl_FindExecutable(argv[0]);
    auto *interp = Tcl_CreateInterp();
    if (Tcl_Init(interp) != TCL_OK || Tk_Init(interp) != TCL_OK) {
        std::cerr << "Cannot start the graphical interface: " << Tcl_GetStringResult(interp) << "\nRun this app from an Ubuntu graphical desktop.\n";
        Tcl_DeleteInterp(interp); Tcl_Finalize(); return 1;
    }
    App app(interp, engine);
    Tcl_CreateNamespace(interp, "native", nullptr, nullptr);
    for (const auto &name : {"enqueue", "quickdownload", "start", "cancel", "login", "browse", "remove", "close", "openlogin"}) {
        const std::string command = std::string("native::") + name;
        Tcl_CreateObjCommand(interp, command.c_str(), dispatch, &app, nullptr);
    }
    if (Tcl_EvalEx(interp, kLayout, -1, TCL_EVAL_GLOBAL) != TCL_OK) {
        std::cerr << "UI initialization failed: " << Tcl_GetStringResult(interp) << '\n';
        if (const char *error = Tcl_GetVar(interp, "errorInfo", TCL_GLOBAL_ONLY)) std::cerr << error << '\n';
        Tcl_DeleteInterp(interp); Tcl_Finalize(); return 1;
    }
    app.initialize();
    if (smokeTest) Tcl_EvalEx(interp, "after 1500 {destroy .}", -1, TCL_EVAL_GLOBAL);
    Tk_MainLoop(); Tcl_DeleteInterp(interp); Tcl_Finalize(); return 0;
}
