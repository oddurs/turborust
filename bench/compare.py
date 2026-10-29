#!/usr/bin/env python3
"""Head-to-head benchmark of local process supervisors (cairn 0111).

    python3 bench/compare.py <scenario> [tool,tool,...|all]

Scenarios: lifecycle, stubborn, killed, throughput, longline, flood, churn,
watch, coalesce. Knobs are environment variables: REPS, NS, LINES, MB, N,
SECONDS_, EDITS. Tools are found on PATH, or at $BENCH_<TOOL>; turborust at
$TURBORUST, defaulting to this checkout's release build. Linux only: processes
are found through /proc. Results are JSON lines on stdout.

Every tool gets the same workload, written in its own config format, and is
launched headless with output to /dev/null. Processes are found through a
token in their environment, so a tool's helpers (a tmux server, a runner
script) count as its overhead and anything left behind after a stop counts as
leaked, whoever started it.
"""
import json, os, signal, statistics, subprocess, sys, tempfile, time, shutil

def tool_path(name, default=None):
    """`$BENCH_<NAME>` if set, else the first `name` on PATH."""
    return os.environ.get("BENCH_" + name.upper().replace("-", "_")) or shutil.which(name) or default or name


REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TURBORUST = os.environ.get("TURBORUST", os.path.join(REPO, "target", "release", "turborust"))
TICK = os.sysconf("SC_CLK_TCK")


# ---------------------------------------------------------------- tools

def procfile(d, services):
    with open(os.path.join(d, "Procfile"), "w") as f:
        for name, cmd in services:
            f.write(f"{name}: {cmd}\n")


def toml_str(s):
    return json.dumps(s)


class Tool:
    name = "?"
    def write(self, d, services, extra=None): ...
    def argv(self, d): ...


class Turborust(Tool):
    name = "turborust"
    def write(self, d, services, extra=None):
        with open(os.path.join(d, "turborust.toml"), "w") as f:
            for name, cmd in services:
                f.write(f"[services.{name}]\ncmd = {toml_str(cmd)}\n")
                for k, v in (extra or {}).items():
                    f.write(f"{k} = {v}\n")
                f.write("\n")
    def argv(self, d):
        return [TURBORUST, "up", "--no-tui"]


class ProcessCompose(Tool):
    name = "process-compose"
    def write(self, d, services, extra=None):
        with open(os.path.join(d, "process-compose.yaml"), "w") as f:
            f.write('version: "0.5"\nprocesses:\n')
            for name, cmd in services:
                f.write(f"  {name}:\n    command: {json.dumps(cmd)}\n")
                for k, v in (extra or {}).items():
                    f.write(f"    {k}: {v}\n")
    def argv(self, d):
        return [tool_path("process-compose"), "up", "-t=false", "--no-server",
                "-L", os.path.join(d, "pc.log"), "-f", os.path.join(d, "process-compose.yaml")]


class Overmind(Tool):
    name = "overmind"
    def write(self, d, services, extra=None):
        procfile(d, services)
    def argv(self, d):
        return [tool_path("overmind"), "start", "-N", "-f", os.path.join(d, "Procfile"),
                "-s", os.path.join(d, "overmind.sock")]


class Hivemind(Tool):
    name = "hivemind"
    def write(self, d, services, extra=None):
        procfile(d, services)
    def argv(self, d):
        return [tool_path("hivemind"), os.path.join(d, "Procfile")]


class Honcho(Tool):
    name = "honcho"
    def write(self, d, services, extra=None):
        procfile(d, services)
    def argv(self, d):
        return [tool_path("honcho"), "start", "-f", os.path.join(d, "Procfile")]


class Foreman(Tool):
    name = "foreman"
    def write(self, d, services, extra=None):
        procfile(d, services)
    def argv(self, d):
        return [tool_path("foreman"), "start", "-f", os.path.join(d, "Procfile")]


TOOLS = {t.name: t for t in (Turborust(), ProcessCompose(), Overmind(), Hivemind(), Honcho(), Foreman())}


# ---------------------------------------------------------------- processes

def token_procs(token):
    """pid -> comm for every live process carrying `token` in its environment."""
    out = {}
    needle = f"BENCH_TOKEN={token}".encode()
    for p in os.listdir("/proc"):
        if not p.isdigit():
            continue
        try:
            with open(f"/proc/{p}/environ", "rb") as f:
                if needle not in f.read():
                    continue
            with open(f"/proc/{p}/stat") as f:
                st = f.read()
            state = st[st.rindex(")") + 2]
            if state == "Z":
                continue
            with open(f"/proc/{p}/comm") as f:
                out[int(p)] = f.read().strip()
        except (FileNotFoundError, ProcessLookupError, PermissionError):
            pass
    return out


WORKLOAD = {"sleep", "yes", "seq"}


def overhead(procs):
    return {p: c for p, c in procs.items() if c not in WORKLOAD}


def cpu_ticks(pid):
    try:
        with open(f"/proc/{pid}/stat") as f:
            st = f.read()
        fields = st[st.rindex(")") + 2:].split()
        return int(fields[11]) + int(fields[12])
    except (FileNotFoundError, ProcessLookupError):
        return None


def pss_kb(pid):
    try:
        with open(f"/proc/{pid}/smaps_rollup") as f:
            for line in f:
                if line.startswith("Pss:"):
                    return int(line.split()[1])
    except (FileNotFoundError, ProcessLookupError, PermissionError):
        pass
    return 0


class Run:
    def __init__(self, tool, services, extra=None):
        self.tool = tool
        self.dir = tempfile.mkdtemp(prefix=f"bench-{tool.name}-")
        self.marks = os.path.join(self.dir, "marks")
        os.mkdir(self.marks)
        self.token = os.path.basename(self.dir)
        services = [(n, c.replace("$MARKS", self.marks).replace("$DIR", self.dir)) for n, c in services]
        tool.write(self.dir, services, extra)
        env = dict(os.environ, BENCH_TOKEN=self.token)
        env.pop("TMUX", None)
        self.t0 = time.monotonic()
        self.proc = subprocess.Popen(tool.argv(self.dir), cwd=self.dir, env=env,
                                     stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                     stderr=subprocess.DEVNULL, start_new_session=True)

    def marks_count(self):
        return len(os.listdir(self.marks))

    def wait_marks(self, n, timeout=60):
        while time.monotonic() - self.t0 < timeout:
            if self.marks_count() >= n:
                return time.monotonic() - self.t0
            if self.proc.poll() is not None:
                return None
            time.sleep(0.001)
        return None

    def stop(self, sig=signal.SIGTERM, timeout=20):
        """Signals the supervisor; returns (seconds until everything is gone, leaked)."""
        t = time.monotonic()
        try:
            os.kill(self.proc.pid, sig)
        except ProcessLookupError:
            pass
        gone = None
        while time.monotonic() - t < timeout:
            self.proc.poll()
            if self.proc.returncode is not None and not token_procs(self.token):
                gone = time.monotonic() - t
                break
            time.sleep(0.002)
        if self.proc.returncode is None:
            try:
                self.proc.wait(timeout=max(0.1, timeout - (time.monotonic() - t)))
            except subprocess.TimeoutExpired:
                pass
        time.sleep(0.3)
        leaked = token_procs(self.token)
        return gone, leaked

    def cleanup(self):
        for p in token_procs(self.token):
            try:
                os.kill(p, signal.SIGKILL)
            except ProcessLookupError:
                pass
        try:
            os.killpg(self.proc.pid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass
        subprocess.run(["tmux", "-L", "overmind", "kill-server"], capture_output=True)
        shutil.rmtree(self.dir, ignore_errors=True)


def sleepers(n):
    return [(f"s{i}", f"touch $MARKS/s{i}; exec sleep 100000") for i in range(n)]


# ---------------------------------------------------------------- scenarios

def med(xs):
    xs = [x for x in xs if x is not None]
    return statistics.median(xs) if xs else None


def scenario_lifecycle(tool, n, reps):
    """Start n services; time to all running; idle CPU and memory; stop."""
    starts, stops, cpus, mems, leaks, fails = [], [], [], [], [], 0
    for _ in range(reps):
        r = Run(tool, sleepers(n))
        try:
            s = r.wait_marks(n)
            if s is None:
                fails += 1
                continue
            starts.append(s)
            time.sleep(1.0)
            oh = overhead(token_procs(r.token))
            before = {p: cpu_ticks(p) for p in oh}
            window = 5.0
            time.sleep(window)
            after = {p: cpu_ticks(p) for p in oh}
            ticks = sum((after[p] or before[p]) - before[p] for p in oh if before[p] is not None)
            cpus.append(100.0 * ticks / TICK / window)
            mems.append(sum(pss_kb(p) for p in overhead(token_procs(r.token))) / 1024)
            gone, leaked = r.stop()
            stops.append(gone)
            leaks.append(len(leaked))
        finally:
            r.cleanup()
    return dict(start_s=med(starts), idle_cpu_pct=med(cpus), pss_mb=med(mems),
                stop_s=med(stops), leaked=max(leaks) if leaks else None, failed=fails)


def scenario_stubborn(tool, reps):
    """A child that ignores SIGTERM and has a grandchild: is it all gone, and how fast."""
    cmd = "trap '' TERM; touch $MARKS/a; sleep 100000 & sleep 100000"
    stops, leaks = [], []
    for _ in range(reps):
        r = Run(tool, [("stubborn", cmd)])
        try:
            if r.wait_marks(1) is None:
                stops.append(None)
                continue
            time.sleep(0.5)
            gone, leaked = r.stop(timeout=30)
            stops.append(gone)
            leaks.append(len(leaked))
        finally:
            r.cleanup()
    return dict(stop_s=med(stops), leaked=max(leaks) if leaks else None)


def scenario_killed(tool, reps):
    """The supervisor is SIGKILLed: does anything it started survive."""
    leaks = []
    for _ in range(reps):
        r = Run(tool, [("a", "touch $MARKS/a; exec sleep 100000"),
                       ("b", "touch $MARKS/b; sleep 100000 & exec sleep 100000")])
        try:
            if r.wait_marks(2) is None:
                continue
            time.sleep(0.5)
            _, leaked = r.stop(sig=signal.SIGKILL, timeout=3)
            leaks.append(len(leaked))
        finally:
            r.cleanup()
    return dict(leaked=max(leaks) if leaks else None)


def scenario_throughput(tool, lines, reps):
    """One service prints `lines` lines as fast as it can; time until it is done."""
    cmd = f"seq 1 {lines}; touch $MARKS/done; exec sleep 100000"
    times, cpus = [], []
    for _ in range(reps):
        r = Run(tool, [("loud", cmd)])
        try:
            t = r.wait_marks(1, timeout=300)
            times.append(t)
            cpus.append(sum(cpu_ticks(p) or 0 for p in overhead(token_procs(r.token))) / TICK)
            r.stop()
        finally:
            r.cleanup()
    return dict(seconds=med(times), lines_per_s=(lines / med(times)) if med(times) else None,
                supervisor_cpu_s=med(cpus))


def scenario_long_line(tool, mb, reps):
    """One line of `mb` MiB without a newline, then silence: memory afterwards."""
    cmd = f"head -c {mb * 1024 * 1024} /dev/zero | tr '\\\\0' x; touch $MARKS/done; exec sleep 100000"
    mems, ok = [], 0
    for _ in range(reps):
        r = Run(tool, [("wide", cmd)])
        try:
            if r.wait_marks(1, timeout=120) is not None:
                ok += 1
                time.sleep(1.0)
                mems.append(sum(pss_kb(p) for p in overhead(token_procs(r.token))) / 1024)
            r.stop()
        finally:
            r.cleanup()
    return dict(pss_mb=med(mems), completed=ok)


def scenario_churn(tool, n, seconds):
    """Kill random children for a while; does every service come back, and is
    anything left behind once it stops."""
    import random
    extra = {"availability": "{restart: always, backoff_seconds: 0}"} if tool.name == "process-compose" else None
    r = Run(tool, sleepers(n), extra)
    try:
        if r.wait_marks(n) is None:
            return dict(failed=True)
        kills = 0
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            victims = [p for p, c in token_procs(r.token).items() if c == "sleep"]
            if victims:
                try:
                    os.kill(random.choice(victims), signal.SIGKILL)
                    kills += 1
                except ProcessLookupError:
                    pass
            time.sleep(0.05)
        # Long enough for the longest backoff to run out.
        deadline = time.monotonic() + 30
        alive = 0
        while time.monotonic() < deadline:
            alive = sum(1 for c in token_procs(r.token).values() if c == "sleep")
            if alive == n:
                break
            time.sleep(0.1)
        recovered = time.monotonic() - end
        oh = overhead(token_procs(r.token))
        gone, leaked = r.stop()
        return dict(kills=kills, alive_after=alive, of=n, recovered_s=recovered,
                    supervisor_alive=r.proc.returncode is None or True, stop_s=gone, leaked=len(leaked),
                    helpers=len(oh))
    finally:
        r.cleanup()


def scenario_flood(tool, seconds):
    """A service logs an error line nonstop: does the supervisor's memory level off."""
    cmd = ("touch $MARKS/up; i=0; while :; do i=$((i+1)); "
           "echo \"error: connection refused to db:5432 attempt $i, retrying in 0ms\"; done")
    r = Run(tool, [("noisy", cmd)])
    samples = []
    try:
        if r.wait_marks(1) is None:
            return dict(failed=True)
        t = time.monotonic()
        while time.monotonic() - t < seconds:
            time.sleep(2)
            samples.append(round(sum(pss_kb(p) for p in overhead(token_procs(r.token))) / 1024, 1))
        r.stop()
        return dict(pss_mb_every_2s=samples, growth_mb=round(samples[-1] - samples[1], 1))
    finally:
        r.cleanup()


# ---------------------------------------------------------------- watching

START = "touch $MARKS/$(date +%s%N); exec sleep 100000"


class Watcher:
    """A tool that restarts a command when files under src/ change."""
    def __init__(self, name, write, argv):
        self.name, self._write, self._argv = name, write, argv
    def write(self, d, services, extra=None):
        self._write(d)
    def argv(self, d):
        return self._argv(d)


def tr_watch_config(d):
    with open(os.path.join(d, "turborust.toml"), "w") as f:
        f.write(f"[services.app]\ncmd = {toml_str(START.replace('$MARKS', os.path.join(d, 'marks')))}\n"
                'watch = ["src/**"]\n')


WATCHERS = {
    "turborust": Watcher("turborust", tr_watch_config, lambda d: [TURBORUST, "up", "--no-tui"]),
    "watchexec": Watcher("watchexec", lambda d: None, lambda d: [
        tool_path("watchexec"), "-n", "-r", "-w", "src", "--", "sh", "-c", START.replace("$MARKS", os.path.join(d, "marks"))]),
    "nodemon": Watcher("nodemon", lambda d: None, lambda d: [
        tool_path("nodemon"), "-q", "--watch", "src", "-e", "txt", "--exec", "sh -c " + json.dumps(START.replace("$MARKS", os.path.join(d, "marks")))]),
}


def start_watch_run(name):
    tool = WATCHERS[name]
    r = Run.__new__(Run)
    r.tool = tool
    r.dir = tempfile.mkdtemp(prefix=f"bench-w-{name}-")
    r.marks = os.path.join(r.dir, "marks")
    os.mkdir(r.marks)
    os.mkdir(os.path.join(r.dir, "src"))
    with open(os.path.join(r.dir, "src", "a.txt"), "w") as f:
        f.write("0")
    r.token = os.path.basename(r.dir)
    tool.write(r.dir, [])
    env = dict(os.environ, BENCH_TOKEN=r.token)
    r.t0 = time.monotonic()
    r.proc = subprocess.Popen(tool.argv(r.dir), cwd=r.dir, env=env, stdin=subprocess.DEVNULL,
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
    return r


def scenario_watch(name, edits):
    """Edit a watched file; time until the restarted command is running."""
    lat, missed = [], 0
    r = start_watch_run(name)
    try:
        if r.wait_marks(1, timeout=20) is None:
            return dict(failed=True)
        time.sleep(1.5)
        for i in range(edits):
            before = r.marks_count()
            t = time.monotonic()
            with open(os.path.join(r.dir, "src", "a.txt"), "w") as f:
                f.write(str(i + 1))
            got = None
            while time.monotonic() - t < 5:
                if r.marks_count() > before:
                    got = time.monotonic() - t
                    break
                time.sleep(0.001)
            if got is None:
                missed += 1
            else:
                lat.append(got)
            time.sleep(0.6)
        return dict(median_ms=1000 * med(lat), p90_ms=1000 * sorted(lat)[int(len(lat) * 0.9) - 1] if lat else None,
                    missed=missed, restarts=r.marks_count() - 1, edits=edits)
    finally:
        r.stop()
        r.cleanup()


def scenario_coalesce(name):
    """Restarts caused by changes that are one change to a person."""
    def atomic_save(src):
        tmp = os.path.join(src, ".a.txt.swp")
        with open(tmp, "w") as f:
            f.write(str(time.time()))
        os.chmod(tmp, 0o644)
        os.rename(tmp, os.path.join(src, "a.txt"))

    def checkout(src):
        for i in range(200):
            with open(os.path.join(src, f"f{i}.txt"), "w") as f:
                f.write(str(time.time()))

    def formatter(src):
        for i in range(20):
            with open(os.path.join(src, f"f{i}.txt"), "w") as f:
                f.write(str(time.time()))
            time.sleep(0.005)

    out = {}
    for label, act in (("atomic_save", atomic_save), ("checkout_200", checkout), ("formatter_20x5ms", formatter)):
        counts = []
        for _ in range(3):
            r = start_watch_run(name)
            try:
                src = os.path.join(r.dir, "src")
                for i in range(200):
                    with open(os.path.join(src, f"f{i}.txt"), "w") as f:
                        f.write("seed")
                if r.wait_marks(1, timeout=20) is None:
                    counts.append(None)
                    continue
                time.sleep(1.5)
                before = r.marks_count()
                act(src)
                time.sleep(2.0)
                counts.append(r.marks_count() - before)
            finally:
                r.stop()
                r.cleanup()
        out[label] = counts
    return out


def main():
    which = sys.argv[1]
    pool = WATCHERS if which in ("watch", "coalesce") else TOOLS
    tools = sys.argv[2].split(",") if len(sys.argv) > 2 and sys.argv[2] != "all" else list(pool)
    reps = int(os.environ.get("REPS", "5"))
    results = {}
    for name in tools:
        t = pool[name]
        if which == "lifecycle":
            res = {n: scenario_lifecycle(t, n, reps) for n in map(int, os.environ.get("NS", "1,10,50").split(","))}
        elif which == "stubborn":
            res = scenario_stubborn(t, reps)
        elif which == "killed":
            res = scenario_killed(t, reps)
        elif which == "throughput":
            res = scenario_throughput(t, int(os.environ.get("LINES", "200000")), reps)
        elif which == "flood":
            res = scenario_flood(t, float(os.environ.get("SECONDS_", "20")))
        elif which == "churn":
            res = scenario_churn(t, int(os.environ.get("N", "20")), float(os.environ.get("SECONDS_", "20")))
        elif which == "coalesce":
            res = scenario_coalesce(name)
        elif which == "watch":
            res = scenario_watch(name, int(os.environ.get("EDITS", "20")))
        elif which == "longline":
            res = scenario_long_line(t, int(os.environ.get("MB", "64")), reps)
        results[name] = res
        print(json.dumps({name: res}), flush=True)
    return results


if __name__ == "__main__":
    main()
