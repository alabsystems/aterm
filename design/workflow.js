/* Executable LAN setup design. No network requests or machine changes. */
(function (root) {
  "use strict";
  var definitions = [
    { id: "shell", name: "Commands & shell", group: "This Mac", requires: [], duration: 1 },
    { id: "tools", name: "Developer tools", group: "This Mac", requires: [], duration: 4 },
    { id: "preferences", name: "Mac preferences", group: "This Mac", requires: [], duration: 2 },
    { id: "permissions", name: "System permissions", group: "This Mac", requires: [], duration: 1 },
    { id: "lan", name: "LAN connection", group: "Your LAN", requires: [], duration: 1 },
    { id: "membership", name: "Join this LAN", group: "Your LAN", requires: ["lan"], duration: 1 },
    { id: "credentials", name: "Sync credentials", group: "Your LAN", requires: ["membership"], duration: 2 },
    { id: "fleet", name: "Fleet & discovery", group: "Your LAN", requires: ["membership"], duration: 1 },
    { id: "commons", name: "Shared data", group: "Your LAN", requires: ["membership"], duration: 1 },
    { id: "fabric", name: "Session messaging", group: "Your LAN", requires: ["fleet"], duration: 1 },
    { id: "repos", name: "Your repositories", group: "Development", requires: ["credentials", "tools"], duration: 3 },
    { id: "agents", name: "Agent accounts", group: "Development", requires: ["credentials", "shell"], duration: 2 }
  ];
  function Engine(saved) {
    this.time = saved ? saved.time : 0;
    this.paused = saved ? saved.paused : false;
    this.online = saved ? saved.online : true;
    this.events = saved ? saved.events.slice() : [];
    this.jobs = definitions.map(function (d) {
      var old = saved && saved.jobs.find(function (j) { return j.id === d.id; });
      return Object.assign({}, d, old || { state: "queued", attempts: 0, remaining: 0, retryAt: 0 });
    });
    this.validate();
  }
  Engine.prototype.validate = function () {
    var self = this, visiting = {}, visited = {};
    function walk(id) {
      var job = self.get(id);
      if (!job) throw new Error("Missing dependency: " + id);
      if (visiting[id]) throw new Error("Dependency cycle: " + id);
      if (visited[id]) return;
      visiting[id] = true;
      job.requires.forEach(walk);
      visiting[id] = false; visited[id] = true;
    }
    this.jobs.forEach(function (j) { walk(j.id); });
  };
  Engine.prototype.get = function (id) { return this.jobs.find(function (j) { return j.id === id; }); };
  Engine.prototype.record = function (job, text) {
    this.events.unshift({ time: this.time, id: job.id, text: text });
    this.events = this.events.slice(0, 100);
  };
  Engine.prototype.blockers = function (job) {
    var self = this;
    return job.requires.filter(function (id) { return self.get(id).state !== "done"; });
  };
  Engine.prototype.reconcile = function () {
    var self = this;
    this.jobs.forEach(function (job) {
      if (job.state === "done" || job.state === "running" || job.state === "action") return;
      if (self.paused) return;
      var blocked = self.blockers(job).length || (job.id === "lan" && !self.online);
      if (blocked) { job.state = "waiting"; return; }
      if (job.retryAt > self.time) { job.state = "retrying"; return; }
      job.state = "running";
      job.attempts++;
      job.remaining = job.duration;
      self.record(job, "Started");
    });
  };
  Engine.prototype.tick = function () {
    if (this.paused) return;
    this.time++;
    var self = this;
    this.jobs.forEach(function (job) {
      if (job.state !== "running") return;
      if (--job.remaining <= 0) {
        job.state = "done";
        self.record(job, "Verified complete");
      }
    });
    this.reconcile();
  };
  Engine.prototype.signal = function (id) {
    var self = this, affected = {};
    function invalidate(key) {
      if (affected[key]) return;
      affected[key] = true;
      var job = self.get(key);
      job.state = "queued"; job.retryAt = 0;
      self.jobs.filter(function (j) { return j.requires.indexOf(key) !== -1; }).forEach(function (j) { invalidate(j.id); });
    }
    invalidate(id);
    this.record(this.get(id), "Dependency changed; rechecking");
    this.reconcile();
  };
  Engine.prototype.fail = function (id) {
    var job = this.get(id);
    job.state = "retrying";
    job.retryAt = this.time + Math.min(30, Math.pow(2, Math.max(1, job.attempts)));
    this.record(job, "Temporary failure; retry scheduled");
    this.reconcile();
  };
  Engine.prototype.permission = function (allowed) {
    var job = this.get("permissions");
    job.state = allowed ? "done" : "action";
    this.record(job, allowed ? "Permission observed; continuing" : "Waiting for macOS permission");
    this.reconcile();
  };
  Engine.prototype.snapshot = function () {
    return JSON.parse(JSON.stringify({ time: this.time, paused: this.paused, online: this.online, events: this.events, jobs: this.jobs }));
  };
  Engine.prototype.complete = function () { return this.jobs.every(function (j) { return j.state === "done"; }); };
  function tests() {
    var passed = [];
    function check(condition, name) { if (!condition) throw new Error(name); passed.push(name); }
    function drain(e, n) { for (var i = 0; i < n; i++) e.tick(); }
    var e = new Engine(); e.reconcile();
    check(e.get("tools").state === "running" && e.get("preferences").state === "running", "independent work starts together");
    check(e.get("repos").state === "waiting", "prerequisites block only their dependents");
    drain(e, 15); check(e.complete(), "LAN admission and all dependents complete without a press");
    e = new Engine(); e.online = false; e.reconcile(); drain(e, 6);
    check(e.get("preferences").state === "done" && e.get("membership").state === "waiting", "offline LAN does not block local setup");
    e.online = true; e.signal("lan"); drain(e, 12); check(e.complete(), "network return resumes dependents");
    e = new Engine(); e.reconcile(); e.fail("tools"); var first = e.get("tools").retryAt;
    check(first > e.time, "temporary failure backs off"); drain(e, 2);
    check(e.get("tools").state === "running", "retry starts without a button");
    for (var j = 0; j < 5; j++) { e.fail("tools"); drain(e, 35); }
    check(e.get("tools").state === "done", "no permanent stop after two failures");
    e = new Engine(); e.reconcile(); e.permission(false); drain(e, 15);
    check(!e.complete() && e.get("repos").state === "done", "OS action is incomplete and does not block unrelated work");
    e.permission(true); check(e.complete(), "observed permission completes without retry press");
    e = new Engine(); e.reconcile(); e.paused = true; var time = e.time; drain(e, 10);
    check(e.time === time, "explicit pause is respected");
    e = new Engine(e.snapshot()); check(e.paused, "pause survives coordinator restart");
    e.paused = false; drain(e, 15); check(e.complete(), "restart resumes durable work");
    var attempts = e.get("preferences").attempts; e.signal("credentials"); drain(e, 10);
    check(e.get("preferences").attempts === attempts, "credential change does not repeat unrelated work");
    return passed;
  }
  root.LanWorkflow = { Engine: Engine, definitions: definitions, tests: tests };
})(typeof globalThis !== "undefined" ? globalThis : this);
