//! One sealed primitive-key / prepared-Reference continuation.
//! Old proved-Number object reads stay on the number-key emitter.
//! Conversion and the role consumer stay nested in the success branch so a
//! Float refinement cannot merge with an unknown sibling.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::javascript) enum Role {
    Get,
    Put,
    Call,
    Delete,
    In,
    Compound,
    Update,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Boolean,
    Number,
    String,
    Null,
    Undefined,
    /// A statically proved ordinary object key converted through the
    /// String-hint ToPrimitive producer.
    Object,
    Dynamic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Receiver {
    Object,
    Function,
    Array,
    Null,
    Undefined,
    Boolean,
    Number,
    String,
    Dynamic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Site {
    role: Role,
    key: Mode,
    receiver: Receiver,
    scope: u32,
    strict: bool,
}

/// One canonical record per member span. The identity triple is fixed by the
/// first observation and never changes; `prepared` is the actual sealed
/// prepared-Reference proof emission may consume; `array_string` is the
/// monotone eligibility of the known-Array + proved-String read/call
/// specialization, true exactly while the stored proof itself is in that
/// domain. An ordinary admitted non-Array proof retracts it for good. A
/// record without `prepared` is observation-only metadata: it publishes no
/// proof, demands no ordinary realm, materializes no helper, roots nothing
/// and selects no consumer.
#[derive(Clone, Copy, Debug)]
struct Entry {
    role: Role,
    scope: u32,
    strict: bool,
    prepared: Option<Site>,
    array_string: bool,
}

#[derive(Default)]
pub(super) struct State {
    sites: std::cell::RefCell<std::collections::HashMap<(u32, u32), Entry>>,
    sealed: std::cell::Cell<bool>,
    /// Loop-entry walks discover effects and publish nothing.
    probe: std::cell::Cell<bool>,
    /// A proved nullish member operation already completed as a throw.
    known_throw: std::cell::Cell<bool>,
    /// Receiver may be nullish or object; the consumer is not definite.
    conditional_throw: std::cell::Cell<bool>,
    /// Joined pre-consumer facts for catch entry.
    throw_edge: std::cell::RefCell<Option<super::expression::BranchFacts>>,
    /// Validation walk: a proved nullish operation cannot complete normally.
    validation_abrupt: std::cell::Cell<bool>,
    /// A selected arm or short-circuit operand may throw. The continuation
    /// still completes when another alternative does not.
    validation_conditional: std::cell::Cell<bool>,
    /// Facts at a possible throw. Empty means the consumer supplies its own.
    validation_throw: std::cell::RefCell<Vec<FactSnap>>,
}

/// Compile-time throw/liveness facts. Probe emission captures this and
/// restores it exactly; it is not a joined edge.
pub(in crate::javascript) struct EffectSnapshot {
    known_throw: bool,
    conditional_throw: bool,
    throw_edge: Option<super::expression::BranchFacts>,
}

impl State {
    fn capture_effects(&self) -> EffectSnapshot {
        EffectSnapshot {
            known_throw: self.known_throw.get(),
            conditional_throw: self.conditional_throw.get(),
            throw_edge: self.throw_edge.borrow().clone(),
        }
    }

    fn restore_effects(&self, saved: EffectSnapshot) {
        self.known_throw.set(saved.known_throw);
        self.conditional_throw.set(saved.conditional_throw);
        *self.throw_edge.borrow_mut() = saved.throw_edge;
    }

    fn freshen_effects(&self) {
        self.known_throw.set(false);
        self.conditional_throw.set(false);
        *self.throw_edge.borrow_mut() = None;
    }

    /// The primitive-key share of the ordinary-realm demand: at least one
    /// prepared Reference proof that emission can actually consume survived the
    /// admission walk. Probe walks register nothing, dead-capability walks
    /// publish nothing, an observation-only record owns no proof, and a
    /// disqualified route drops the proof it used to hold, so none of those can
    /// demand the realm through this query.
    fn publishes_realm_demand(&self) -> bool {
        self.sites
            .borrow()
            .values()
            .any(|entry| entry.prepared.is_some())
    }
}

/// One binding fact captured at a possible throw. Profile maps stay in the
/// validation walk; this snapshot is only the abrupt-edge carrier.
#[derive(Clone, Copy)]
pub(in crate::javascript) struct FactSnap {
    pub id: scope::BindingId,
    pub kind: Kind,
    pub array_length: Option<super::ArrayShape>,
    pub guarded_numeric: bool,
}

pub(in crate::javascript) const OBJECT_KEY_REFUSAL: &str =
    "primitive computed keys reject object ToPrimitive";

fn span_key(span: swc_common::Span) -> (u32, u32) {
    (span.lo.0, span.hi.0)
}

fn mode_covers(stored: Mode, observed: Mode) -> bool {
    stored == observed || stored == Mode::Dynamic
}

fn receiver_covers(stored: Receiver, observed: Receiver) -> bool {
    stored == observed || stored == Receiver::Dynamic
}

/// Whether this site's proof is the known-Array + proved-String read/call
/// specialization itself. Eligibility is a property of the stored proof, not
/// of admission order: an ordinary Object/Function/Dynamic/nullish proof at
/// the same span is a different route and must never keep or revive it.
fn admits_array_string(site: Site) -> bool {
    site.receiver == Receiver::Array
        && site.key == Mode::String
        && matches!(site.role, Role::Get | Role::Call)
}

fn register(
    sites: &mut std::collections::HashMap<(u32, u32), Entry>,
    sealed: bool,
    probe: bool,
    key: (u32, u32),
    site: Site,
) -> Result<(), &'static str> {
    if let Some(existing) = sites.get_mut(&key) {
        if existing.role != site.role {
            return Err("primitive key site role changed");
        }
        if existing.scope != site.scope {
            return Err("primitive key site activation changed");
        }
        if existing.strict != site.strict {
            return Err("primitive key site strictness changed");
        }
        if probe {
            // Probe walks publish and mutate nothing. Coverage is judged
            // against the sealed proof only; an observation-only record owns
            // none, so it already covers every conservative mode.
            let Some(prepared) = existing.prepared else {
                return Ok(());
            };
            if mode_covers(prepared.key, site.key)
                && receiver_covers(prepared.receiver, site.receiver)
            {
                return Ok(());
            }
            return Err("primitive key probe does not cover the sealed conservative mode");
        }
        let array_string = admits_array_string(site);
        let Some(prepared) = existing.prepared else {
            // The first observation may have been an ineligible route that
            // published no proof. A later eligible observation still seals
            // its own ordinary proof normally; only the known-Array/String
            // read is gated by the span's recorded eligibility, which
            // `admit_primitive_member` checks before reaching here.
            if sealed {
                return Err(
                    "primitive key site was not proved before the runtime index window sealed",
                );
            }
            existing.prepared = Some(site);
            return Ok(());
        };
        if !sealed && existing.array_string && !array_string {
            // An ordinary admitted non-Array observation is disqualifying even
            // though it carries its own prepared proof: retract the
            // known-Array/String specialization first, then keep the ordinary
            // proof with the consumer its own receiver/key domain selects. A
            // later Array/String candidate is then blocked by eligibility
            // instead of conflicting with that ordinary proof.
            existing.array_string = false;
            existing.prepared = Some(site);
            return Ok(());
        }
        if prepared.key != site.key {
            return Err("primitive key site mode changed");
        }
        if prepared.receiver != site.receiver {
            return Err("primitive key site receiver mode changed");
        }
        return Ok(());
    }
    if probe {
        return Ok(());
    }
    if sealed {
        return Err("primitive key site was not proved before the runtime index window sealed");
    }
    sites.insert(
        key,
        Entry {
            role: site.role,
            scope: site.scope,
            strict: site.strict,
            prepared: Some(site),
            // Eligibility follows the stored proof: only a proved Array
            // receiver with a proved String key in a read/call position keeps
            // the narrow specialization. An ordinary admitted proof starts
            // ineligible, and a route observation can only retract it further.
            array_string: admits_array_string(site),
        },
    );
    Ok(())
}

fn key_mode(kind: Kind) -> Option<Mode> {
    match kind {
        Kind::Boolean => Some(Mode::Boolean),
        Kind::Number => Some(Mode::Number),
        Kind::String => Some(Mode::String),
        Kind::Null => Some(Mode::Null),
        Kind::Undefined => Some(Mode::Undefined),
        Kind::Object => Some(Mode::Object),
        Kind::Dynamic => Some(Mode::Dynamic),
        _ => None,
    }
}

fn receiver_mode(kind: Kind) -> Option<Receiver> {
    match kind {
        Kind::Object => Some(Receiver::Object),
        Kind::Function => Some(Receiver::Function),
        Kind::Array => Some(Receiver::Array),
        Kind::Null => Some(Receiver::Null),
        Kind::Undefined => Some(Receiver::Undefined),
        Kind::Boolean => Some(Receiver::Boolean),
        Kind::Number => Some(Receiver::Number),
        Kind::String => Some(Receiver::String),
        Kind::Dynamic => Some(Receiver::Dynamic),
    }
}

fn object_like(receiver: Receiver) -> bool {
    matches!(
        receiver,
        Receiver::Object | Receiver::Function | Receiver::Array
    )
}

fn nullish(receiver: Receiver) -> bool {
    matches!(receiver, Receiver::Null | Receiver::Undefined)
}

type PreparedMember = (Vec<Stmt>, Vec<u16>, u16, u16, u16);

impl Lowerer<'_> {
    pub(in crate::javascript) fn emission_known_throw(&self) -> bool {
        self.primitive_key.known_throw.get()
    }

    pub(in crate::javascript) fn set_emission_known_throw(&mut self, value: bool) {
        self.primitive_key.known_throw.set(value);
    }

    pub(in crate::javascript) fn take_emission_known_throw(&mut self) -> bool {
        self.primitive_key.known_throw.replace(false)
    }

    #[allow(dead_code)]
    pub(in crate::javascript) fn emission_conditional_throw(&self) -> bool {
        self.primitive_key.conditional_throw.get()
    }

    pub(in crate::javascript) fn begin_emission_function(&mut self) {
        self.primitive_key.known_throw.set(false);
        self.primitive_key.conditional_throw.set(false);
        *self.primitive_key.throw_edge.borrow_mut() = None;
    }

    pub(in crate::javascript) fn note_emission_throw(&mut self) {
        self.primitive_key.known_throw.set(true);
        self.note_throw_edge();
    }

    fn note_throw_edge(&mut self) {
        let current = super::expression::BranchFacts::save(self);
        let mut slot = self.primitive_key.throw_edge.borrow_mut();
        *slot = Some(match slot.take() {
            Some(prev) => current.joined(&prev),
            None => current,
        });
    }

    pub(in crate::javascript) fn take_throw_edge(
        &mut self,
    ) -> Option<super::expression::BranchFacts> {
        self.primitive_key.throw_edge.borrow_mut().take()
    }

    /// Evaluate a region that may not run. An inner definite throw becomes a
    /// possible throw and does not suppress the outer continuation. The bool
    /// is whether this region itself definitely threw.
    pub(in crate::javascript) fn isolate_conditional_emission<T>(
        &mut self,
        eval: impl FnOnce(&mut Self) -> Result<T, ParseError>,
    ) -> Result<(T, bool), ParseError> {
        let outer_known = self.primitive_key.known_throw.replace(false);
        let outer_conditional = self.primitive_key.conditional_throw.replace(false);
        let outer_edge = self.primitive_key.throw_edge.borrow_mut().take();
        let result = eval(self)?;
        let inner_known = self.primitive_key.known_throw.replace(false);
        let inner_conditional = self.primitive_key.conditional_throw.replace(false);
        let inner_edge = self.primitive_key.throw_edge.borrow_mut().take();
        self.primitive_key.known_throw.set(outer_known);
        let possible = inner_known || inner_conditional;
        self.primitive_key
            .conditional_throw
            .set(outer_conditional || possible);
        if let Some(edge) = outer_edge {
            self.restore_throw_edge(Some(edge));
        }
        if possible && let Some(edge) = inner_edge {
            self.restore_throw_edge(Some(edge));
        }
        Ok((result, inner_known))
    }

    pub(in crate::javascript) fn set_validation_possible(&self, snaps: Vec<FactSnap>) {
        self.primitive_key.validation_conditional.set(true);
        *self.primitive_key.validation_throw.borrow_mut() = snaps;
    }

    pub(in crate::javascript) fn take_validation_conditional(&self) -> bool {
        self.primitive_key.validation_conditional.replace(false)
    }

    pub(in crate::javascript) fn take_validation_throw(&self) -> Vec<FactSnap> {
        std::mem::take(&mut *self.primitive_key.validation_throw.borrow_mut())
    }

    pub(in crate::javascript) fn clear_validation_conditional(&self) {
        self.primitive_key.validation_conditional.set(false);
        self.primitive_key.validation_throw.borrow_mut().clear();
    }

    pub(in crate::javascript) fn restore_throw_edge(
        &mut self,
        edge: Option<super::expression::BranchFacts>,
    ) {
        if let Some(edge) = edge {
            let mut slot = self.primitive_key.throw_edge.borrow_mut();
            *slot = Some(match slot.take() {
                Some(prev) => edge.joined(&prev),
                None => edge,
            });
        }
    }

    pub(in crate::javascript) fn capture_emission_effects(&self) -> EffectSnapshot {
        self.primitive_key.capture_effects()
    }

    pub(in crate::javascript) fn restore_emission_effects(&mut self, saved: EffectSnapshot) {
        self.primitive_key.restore_effects(saved);
    }

    pub(in crate::javascript) fn freshen_emission_effects(&mut self) {
        self.primitive_key.freshen_effects();
    }

    /// Finally always runs. Incoming compile-time throw facts must not make
    /// that body unreachable, and must come back when the body does not
    /// itself definitely throw. Runtime Completion slots are not touched.
    pub(in crate::javascript) fn isolate_finally_effects<T>(
        &mut self,
        body: impl FnOnce(&mut Self) -> Result<T, ParseError>,
    ) -> Result<T, ParseError> {
        let saved = self.capture_emission_effects();
        self.freshen_emission_effects();
        let result = body(self);
        if result.is_err() {
            self.restore_emission_effects(saved);
            return result;
        }
        let body_known = self.primitive_key.known_throw.get();
        let body_conditional = self.primitive_key.conditional_throw.get();
        let body_edge = self.primitive_key.throw_edge.borrow_mut().take();
        self.restore_emission_effects(saved);
        if body_known {
            self.primitive_key.known_throw.set(true);
        }
        if body_conditional {
            self.primitive_key.conditional_throw.set(true);
        }
        if let Some(edge) = body_edge {
            self.restore_throw_edge(Some(edge));
        }
        result
    }

    pub(in crate::javascript) fn validation_abrupt(&self) -> bool {
        self.primitive_key.validation_abrupt.get()
    }

    pub(in crate::javascript) fn set_validation_abrupt(&self, value: bool) {
        self.primitive_key.validation_abrupt.set(value);
    }

    pub(in crate::javascript) fn open_computed_key(member: &ast::MemberExpr) -> Option<&ast::Expr> {
        let ast::MemberProp::Computed(property) = &member.prop else {
            return None;
        };
        if matches!(
            Self::unwrap_parens(&property.expr),
            ast::Expr::Lit(ast::Lit::Str(_))
        ) {
            return None;
        }
        // Integer indexes stay off this opener only when the receiver is an
        // array; admission rejects that domain. Literal numbers on
        // object/function/nullish/dynamic receivers are previously refused
        // Number keys and must be sealed here.
        Some(&property.expr)
    }

    pub(in crate::javascript) fn in_primitive_key_probe(&self) -> bool {
        self.primitive_key.probe.get()
    }

    pub(in crate::javascript) fn probe_primitive_key<T>(
        &self,
        validate: impl FnOnce() -> Result<T, ParseError>,
    ) -> Result<T, ParseError> {
        let previous = self.primitive_key.probe.replace(true);
        let result = validate();
        self.primitive_key.probe.set(previous);
        result
    }

    fn record_site(
        &self,
        span: swc_common::Span,
        role: Role,
        key: Mode,
        receiver: Receiver,
    ) -> Result<(), ParseError> {
        let scope = self.catch_overlay.get().unwrap_or(self.active_scope).0;
        let site = Site {
            role,
            key,
            receiver,
            scope,
            strict: self.enclosing_strict,
        };
        register(
            &mut self.primitive_key.sites.borrow_mut(),
            self.primitive_key.sealed.get(),
            self.primitive_key.probe.get(),
            span_key(span),
            site,
        )
        .map_err(|message| self.error(span, message))
    }

    /// Whether the known-Array + proved-String read/call specialization is
    /// still admissible at this span. An entry whose stored prepared proof is
    /// outside that domain (an ordinary Object/Function/Dynamic/nullish proof)
    /// was never eligible, and any earlier observation that took a different
    /// route disarmed it for the rest of the compilation, even when that
    /// observation itself could not publish a prepared proof.
    fn member_route_eligible(&self, span: swc_common::Span) -> bool {
        self.primitive_key
            .sites
            .borrow()
            .get(&span_key(span))
            .is_none_or(|entry| entry.array_string)
    }

    /// Record that this span was observed on a route other than the known
    /// Array + proved String read/call one. The disqualification is monotone
    /// and selective: a published Array/String proof is dropped, the span
    /// identity stays, and every other receiver/key proof keeps its own
    /// registration and conflict checks. Probe and dead-capability walks are
    /// capability-only, so they never mutate the record.
    fn disarm_member_route(&self, span: swc_common::Span, role: Role) {
        if self.primitive_key.probe.get() || self.in_dead_capability() {
            return;
        }
        let scope = self.catch_overlay.get().unwrap_or(self.active_scope).0;
        let strict = self.enclosing_strict;
        let mut sites = self.primitive_key.sites.borrow_mut();
        if let Some(existing) = sites.get_mut(&span_key(span)) {
            if existing.role != role || existing.scope != scope || existing.strict != strict {
                return;
            }
            existing.array_string = false;
            if existing
                .prepared
                .is_some_and(|proof| proof.receiver == Receiver::Array)
            {
                existing.prepared = None;
            }
            return;
        }
        // An observation-only record publishes no prepared proof, so it stays
        // legal after the sealing window; it can only remove eligibility.
        sites.insert(
            span_key(span),
            Entry {
                role,
                scope,
                strict,
                prepared: None,
                array_string: false,
            },
        );
    }

    /// `Ok(true)` means this member was sealed and the caller must not use the
    /// identifier/string/old-number-read path. `Ok(false)` means delegate.
    pub(in crate::javascript) fn admit_primitive_member(
        &self,
        member: &ast::MemberExpr,
        base_kind: Kind,
        key_kind: Kind,
        role: Role,
    ) -> Result<bool, ParseError> {
        let Some(key_expr) = Self::open_computed_key(member) else {
            return Ok(false);
        };
        let Some(receiver) = receiver_mode(base_kind) else {
            return Ok(false);
        };
        // A known Array takes the same prepared Reference only for a proved
        // String key in a read position: its own index/`length` surface and
        // the actual prototype chain stay inside the ordinary runtime Get.
        // Every other Array key kind (Number indexes included) keeps its
        // existing specialization, write and refusal routes.
        let array_string_name = receiver == Receiver::Array
            && key_kind == Kind::String
            && matches!(role, Role::Get | Role::Call);
        if !matches!(
            receiver,
            Receiver::Object
                | Receiver::Function
                | Receiver::Null
                | Receiver::Undefined
                | Receiver::Dynamic
        ) && !array_string_name
        {
            // A read/call observation on any other receiver route (another
            // key kind, a write/delete role, or a primitive receiver) is
            // ineligible for the known-Array/String prepared read. Record it
            // so a String pass seen later cannot keep or publish that proof.
            if matches!(role, Role::Get | Role::Call) {
                self.disarm_member_route(member.span, role);
            }
            return Ok(false);
        }
        if matches!(key_kind, Kind::Array | Kind::Function) {
            return Err(self.error(key_expr.span(), OBJECT_KEY_REFUSAL));
        }
        let Some(mode) = key_mode(key_kind) else {
            return Err(self.error(
                key_expr.span(),
                "primitive computed keys require a primitive or dynamic key",
            ));
        };
        if role == Role::Get
            && base_kind == Kind::Object
            && key_kind == Kind::Number
            && Self::number_key_candidate(member)
        {
            // The ordinary object Number peer is still an observation at this
            // span; it must disarm the Array/String read instead of letting a
            // later String pass publish a narrow proof over it.
            self.disarm_member_route(member.span, role);
            return Ok(false);
        }
        if array_string_name && !self.member_route_eligible(member.span) {
            // An ineligible observation already took this span out of the
            // narrow Array/String domain: leave the member on its legacy
            // numeric/dense route instead of preparing a Reference.
            return Ok(false);
        }
        if self.in_dead_capability() {
            // Unreachable text is admitted for capability only. It publishes
            // no prepared proof, no eligibility change and no ordinary-realm
            // demand; emission never reaches this member, so charging the
            // realm for it would be a dead fee.
            return Ok(true);
        }
        self.record_site(member.span, role, mode, receiver)?;
        Ok(true)
    }

    pub(in crate::javascript) fn admit_primitive_in(
        &self,
        binary: &ast::BinExpr,
        key_kind: Kind,
        right_kind: Kind,
    ) -> Result<bool, ParseError> {
        if matches!(key_kind, Kind::String | Kind::Undefined) {
            return Ok(false);
        }
        if matches!(key_kind, Kind::Array | Kind::Function) {
            return Err(self.error(binary.left.span(), OBJECT_KEY_REFUSAL));
        }
        let Some(mode) = key_mode(key_kind) else {
            return Err(self.error(
                binary.left.span(),
                "P7 in keys require a proved String or undefined",
            ));
        };
        let receiver = receiver_mode(right_kind).ok_or_else(|| {
            self.error(
                binary.right.span(),
                "primitive in requires a sealed right operand",
            )
        })?;
        if self.in_dead_capability() {
            // Capability-only admission: dead text neither publishes a proof
            // nor demands the ordinary realm that would consume it.
            return Ok(true);
        }
        self.record_site(binary.span, Role::In, mode, receiver)?;
        Ok(true)
    }

    /// Emission-side capability boundary shared by every already-admitted
    /// Reference role in discarded text. The caller has already lowered the
    /// receiver and key so their syntax is checked; this re-runs the canonical
    /// receiver/key admission in probe/dead isolation and reports whether the
    /// route that the live walk seals with a prepared proof owns this role.
    /// It never records a site, publishes demand, materializes a Reference or
    /// disarms another observation, so discarded IR carries no executable
    /// proof and no dead fee.
    pub(in crate::javascript) fn discard_member_capability(
        &self,
        member: &ast::MemberExpr,
        base_kind: Kind,
        key_kind: Kind,
        role: Role,
    ) -> Result<bool, ParseError> {
        if !self.in_dead_capability() && !self.emission_known_throw() {
            return Ok(false);
        }
        if self.primitive_key_role(member.span, role)
            || Self::open_computed_key(member).is_none()
        {
            return Ok(false);
        }
        self.probe_dead_capability(|| {
            self.admit_primitive_member(member, base_kind, key_kind, role)
        })
    }

    /// The `in` peer of [`Self::discard_member_capability`]. A proved
    /// String/Undefined key keeps its legacy route, so only the admitted
    /// primitive-key key domains answer `Ok(true)`.
    pub(in crate::javascript) fn discard_in_capability(
        &self,
        binary: &ast::BinExpr,
        key_kind: Kind,
        right_kind: Kind,
    ) -> Result<bool, ParseError> {
        if !self.in_dead_capability() && !self.emission_known_throw() {
            return Ok(false);
        }
        if self.primitive_key_role(binary.span, Role::In) {
            return Ok(false);
        }
        self.probe_dead_capability(|| self.admit_primitive_in(binary, key_kind, right_kind))
    }

    pub(super) fn primitive_key_get_mode(&self, member: &ast::MemberExpr) -> Option<()> {
        self.site_role(member.span, Role::Get).map(|_| ())
    }

    pub(super) fn primitive_key_put_mode(&self, member: &ast::MemberExpr) -> Option<()> {
        self.site_role(member.span, Role::Put).map(|_| ())
    }

    /// A sealed Dynamic receiver with a proved Number key is the only site
    /// that can reach a call-result/conditional array at runtime. Such a
    /// site needs the array/ordinary two-way dispatch instead of the plain
    /// string-key Reference path.
    pub(super) fn primitive_key_dynamic_number(
        &self,
        member: &ast::MemberExpr,
        role: Role,
    ) -> bool {
        self.site_role(member.span, role)
            .is_some_and(|site| site.receiver == Receiver::Dynamic && site.key == Mode::Number)
    }

    /// Re-check that this exact span was sealed for the given role in the
    /// current activation before emitting an array-specific consumer.
    pub(super) fn require_primitive_site(
        &self,
        span: swc_common::Span,
        role: Role,
    ) -> Result<(), ParseError> {
        self.demand_site(span, role).map(|_| ())
    }

    pub(super) fn primitive_key_role(&self, span: swc_common::Span, role: Role) -> bool {
        self.site_role(span, role).is_some()
    }

    fn site_role(&self, span: swc_common::Span, role: Role) -> Option<Site> {
        if !self.primitive_key.sealed.get() {
            return None;
        }
        let entry = self
            .primitive_key
            .sites
            .borrow()
            .get(&span_key(span))
            .copied()?;
        if entry.role != role {
            return None;
        }
        // An observation-only or disqualified record owns no prepared proof,
        // so emission takes the legacy route exactly as if this span had never
        // been admitted.
        entry.prepared
    }

    fn demand_site(&self, span: swc_common::Span, role: Role) -> Result<Site, ParseError> {
        let site = self
            .site_role(span, role)
            .ok_or_else(|| self.error(span, "primitive key site missing at emission"))?;
        if site.scope != self.active_scope.0 || site.strict != self.enclosing_strict {
            return Err(self.error(span, "primitive key activation does not match emission"));
        }
        Ok(site)
    }

    /// The complete ordinary-realm demand consumed when the script (or a
    /// function body's owning realm) picks the heap-free versus ordinary route:
    /// the pre-existing independent producer cell plus a surviving primitive-key
    /// prepared proof. Admission itself never widens the cell, so a disarmed,
    /// probe or dead observation cannot demand the realm; an independent
    /// producer (Object/Function/builtin/Math/URI/JSON/Date/Error/throw helper,
    /// string length, member write, ...) is kept exactly as before. An
    /// observation-only or disqualified record owns no prepared proof and adds
    /// nothing here.
    pub(in crate::javascript) fn ordinary_realm_demand(&self) -> bool {
        self.needs_ordinary_realm.get() || self.primitive_key.publishes_realm_demand()
    }

    pub(super) fn append_primitive_key_prerequisites(
        &mut self,
        ir: &mut FrontendIr,
        carrier_schema: &TypeSchema,
        span: swc_common::Span,
        statements: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        self.primitive_key.sealed.set(true);
        let sites = self.primitive_key.sites.borrow();
        // Observation-only records own no prepared proof, so they must never
        // pull a number/fixed helper or any other runtime cost into the
        // window. Only a real sealed proof drives the prerequisites.
        let needs_number = sites
            .values()
            .filter_map(|entry| entry.prepared)
            .any(|site| matches!(site.key, Mode::Number | Mode::Dynamic));
        let needs_fixed = sites.values().filter_map(|entry| entry.prepared).any(|site| {
            matches!(
                site.key,
                Mode::Boolean | Mode::Null | Mode::Undefined | Mode::Dynamic
            )
        });
        drop(sites);
        if needs_number {
            self.ensure_number_string_prerequisites(ir, span)?;
        }
        if needs_fixed {
            self.append_fixed_scalar_string_helper(ir, carrier_schema, span, statements)?;
        }
        Ok(())
    }

    fn tag_eq(slot: u16, tag: i64) -> Expr {
        Expr::Eq(
            Box::new(Self::as_number(value::field(slot, "tag"))),
            Box::new(Self::as_number(Expr::Int(tag))),
        )
    }

    pub(super) fn nullish_tag(slot: u16) -> Expr {
        Expr::Or(
            Box::new(Self::tag_eq(slot, value::NULL)),
            Box::new(Self::tag_eq(slot, value::UNDEFINED)),
        )
    }

    fn gate_stmts(&self, stmts: Vec<Stmt>, span: swc_common::Span) -> Vec<Stmt> {
        match self.pending_normal {
            Some(normal) => vec![Stmt::IfElse {
                condition: Expr::Var(normal),
                then_branch: stmts,
                else_branch: Vec::new(),
                line: self.line(span),
            }],
            None => stmts,
        }
    }

    fn append_type_error(
        &mut self,
        span: swc_common::Span,
        stmts: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        self.emit_proved_nullish_put_error(span, stmts)
    }

    /// ES5 CheckObjectCoercible for a proved nullish base, before key
    /// conversion and before RHS. Object/Function callers must not use this.
    pub(super) fn emit_proved_nullish_put_error(
        &mut self,
        span: swc_common::Span,
        statements: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        let carrier = self.active_carrier(span)?;
        let intrinsic = self
            .intrinsics
            .ok_or_else(|| self.error(span, "ordinary runtime missing"))?
            .type_error;
        let thrown = super::super::prelude::object::result(
            value::value_result_schema(),
            false,
            Self::guest_call(intrinsic, vec![Expr::Var(carrier)]),
            self.ir_span(span),
        );
        let mut failure = Vec::new();
        self.consume_value_result(thrown, span, &mut failure)?;
        if let Some(normal) = self.pending_normal {
            statements.push(Stmt::IfElse {
                condition: Expr::Var(normal),
                then_branch: failure,
                else_branch: Vec::new(),
                line: self.line(span),
            });
        } else {
            statements.extend(failure);
        }
        Ok(())
    }

    fn append_key_conversion(
        &mut self,
        mode: Mode,
        input: u16,
        key_slot: u16,
        span: swc_common::Span,
        stmts: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        let line = self.line(span);
        let carrier = self.active_carrier(span)?;
        let frame = self.active_frame(span)?;
        let entry = self.entry_for(key_slot, span)?;
        match mode {
            Mode::String => {
                stmts.push(Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: key_slot,
                    expr: Expr::Var(input),
                    line,
                });
                Ok(())
            }
            Mode::Number => {
                self.assign_number_key(carrier, frame, entry, input, key_slot, span, stmts)
            }
            Mode::Boolean | Mode::Null | Mode::Undefined => {
                self.assign_fixed_key(carrier, frame, entry, input, key_slot, span, stmts)
            }
            Mode::Dynamic => {
                let mut number_arm = Vec::new();
                self.assign_number_key(
                    carrier,
                    frame,
                    entry,
                    input,
                    key_slot,
                    span,
                    &mut number_arm,
                )?;
                let string_arm = vec![Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: key_slot,
                    expr: Expr::Var(input),
                    line,
                }];
                let mut fixed_arm = Vec::new();
                self.assign_fixed_key(
                    carrier,
                    frame,
                    entry,
                    input,
                    key_slot,
                    span,
                    &mut fixed_arm,
                )?;
                let fatal = vec![super::assert_stmt(Expr::Bool(false), line)];
                let mut object_arm = Vec::new();
                self.append_key_conversion(Mode::Object, input, key_slot, span, &mut object_arm)?;
                let (helper_read, checked_base) = {
                    let heap = self.heap_ref(span)?;
                    (heap.helper_read, heap.helpers.checked_base)
                };
                let decode = self.helper_decode.ok_or_else(|| {
                    self.error(
                        span,
                        "primitive Dynamic key requires the realm decode helper",
                    )
                })?;
                let (base, base_saved) = self.bind_typed(
                    Self::guest_call(
                        checked_base,
                        vec![
                            Expr::Var(carrier),
                            value::field(input, "ref_slot"),
                            value::field(input, "generation"),
                        ],
                    ),
                    TypeSchema::Int,
                    span,
                )?;
                let (kind, kind_saved) = self.bind_typed(
                    Self::guest_call(
                        decode,
                        vec![Self::guest_call(
                            helper_read,
                            vec![Expr::Var(carrier), Self::as_number(Expr::Var(base))],
                        )],
                    ),
                    TypeSchema::Int,
                    span,
                )?;
                let mut checked_object_arm = vec![base_saved, kind_saved];
                checked_object_arm.push(Stmt::IfElse {
                    condition: Expr::Eq(
                        Box::new(Expr::Var(kind)),
                        Box::new(Expr::Int(layout::KIND_OBJECT)),
                    ),
                    then_branch: object_arm,
                    else_branch: fatal.clone(),
                    line,
                });
                let nested = |tag, arm: Vec<Stmt>, else_branch| Stmt::IfElse {
                    condition: Self::tag_eq(input, tag),
                    then_branch: arm,
                    else_branch,
                    line,
                };
                stmts.push(nested(
                    value::NUMBER,
                    number_arm,
                    vec![nested(
                        value::STRING,
                        string_arm,
                        vec![nested(
                            value::BOOLEAN,
                            fixed_arm.clone(),
                            vec![nested(
                                value::NULL,
                                fixed_arm.clone(),
                                vec![nested(
                                    value::UNDEFINED,
                                    fixed_arm,
                                    vec![nested(value::OBJECT, checked_object_arm, fatal)],
                                )],
                            )],
                        )],
                    )],
                ));
                Ok(())
            }
            Mode::Object => {
                // The String-hint ToPrimitive of an ordinary object key can
                // throw from toString/valueOf. `key_string` hard-codes its
                // scratch roots at KEY_ENTRY+1..3, so it gets a dedicated
                // frame instead of the activation frame that roots base/key.
                let root_enter = self.heap_ref(span)?.helpers.root_enter;
                let root_leave = self.heap_ref(span)?.helpers.root_leave;
                let (scratch, saved) = self.bind_typed(
                    Self::guest_call(root_enter, vec![Expr::Var(carrier), Expr::Int(8)]),
                    TypeSchema::Int,
                    span,
                )?;
                stmts.push(saved);
                let key_string = self
                    .objects
                    .ok_or_else(|| self.error(span, "ordinary runtime missing"))?
                    .key_string;
                let produced = self.consume_value_result(
                    Self::guest_call(
                        key_string,
                        vec![Expr::Var(carrier), Expr::Var(input), Expr::Var(scratch)],
                    ),
                    span,
                    stmts,
                )?;
                // Publish the converted String into the caller's rooted key
                // slot before the scratch frame leaves. A managed Err leaves
                // `produced` Undefined and never overwrites the throw payload.
                stmts.push(Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: key_slot,
                    expr: Expr::Var(produced),
                    line,
                });
                self.refresh_root(key_slot, span, stmts)?;
                // root_leave neither allocates nor collects, so it stays
                // unconditional on both the managed-Ok and Err paths.
                let (_leave_ack, saved) = self.bind_typed(
                    Self::guest_call(root_leave, vec![Expr::Var(carrier), Expr::Var(scratch)]),
                    TypeSchema::Int,
                    span,
                )?;
                stmts.push(saved);
                Ok(())
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn assign_number_key(
        &mut self,
        carrier: u16,
        frame: u16,
        entry: u16,
        input: u16,
        key_slot: u16,
        span: swc_common::Span,
        stmts: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        let managed = self
            .number_string_entries
            .as_ref()
            .ok_or_else(|| {
                self.error(
                    span,
                    "primitive Number key prerequisites must precede the runtime index window",
                )
            })?
            .managed;
        stmts.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: key_slot,
            expr: Self::guest_call(
                managed,
                vec![
                    Expr::Var(carrier),
                    Self::as_number(super::super::prelude::math::checked_number(Expr::Var(
                        input,
                    ))),
                    Expr::Var(frame),
                    Expr::Int(i64::from(entry)),
                ],
            ),
            line: self.line(span),
        });
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn assign_fixed_key(
        &mut self,
        carrier: u16,
        frame: u16,
        entry: u16,
        input: u16,
        key_slot: u16,
        span: swc_common::Span,
        stmts: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        let fixed = self.helper_fixed_scalar_string.ok_or_else(|| {
            self.error(
                span,
                "primitive fixed key prerequisites must precede the runtime index window",
            )
        })?;
        stmts.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: key_slot,
            expr: Self::guest_call(
                fixed,
                vec![
                    Expr::Var(carrier),
                    Expr::Var(input),
                    Expr::Var(frame),
                    Expr::Int(i64::from(entry)),
                ],
            ),
            line: self.line(span),
        });
        Ok(())
    }

    /// Evaluate the original base, then the key. Inside the both-normal
    /// continuation, CheckObjectCoercible precedes conversion and `consumer`.
    fn with_prepared_member<F>(
        &mut self,
        member: &ast::MemberExpr,
        role: Role,
        consumer: F,
    ) -> Result<PreparedMember, ParseError>
    where
        F: FnOnce(&mut Self, u16, u16, u16, &mut Vec<Stmt>) -> Result<(), ParseError>,
    {
        let span = member.span;
        let site = self.demand_site(span, role)?;
        let ast::MemberProp::Computed(property) = &member.prop else {
            unreachable!("sealed computed primitive key")
        };
        let base = self.expr(&member.obj)?;
        if self.emission_known_throw() {
            let mut owned = base.owned_roots;
            let mut body = Vec::new();
            let (base_slot, init) = self.bind_temp(base.expr, member.obj.span())?;
            body.extend(init);
            owned.push(base_slot);
            let (key_slot, init) = self.bind_temp(value::undefined(), span)?;
            body.extend(init);
            owned.push(key_slot);
            let (reference, saved) = self.bind_typed(
                value::reference(
                    value::undefined(),
                    Expr::Int(0),
                    Expr::Int(0),
                    Expr::Bool(self.enclosing_strict),
                ),
                value::reference_schema(),
                span,
            )?;
            body.push(saved);
            return Ok((body, owned, base_slot, key_slot, reference));
        }
        reference::admit_object_receiver(self, span, base.kind)?;
        let mut owned = base.owned_roots;
        let (base_slot, mut body) = self.bind_temp(base.expr, member.obj.span())?;
        owned.push(base_slot);
        let (key_slot, init) = self.bind_temp(value::undefined(), span)?;
        body.extend(init);
        owned.push(key_slot);
        let (reference, saved) = self.bind_typed(
            value::reference(
                value::undefined(),
                Expr::Int(0),
                Expr::Int(0),
                Expr::Bool(self.enclosing_strict),
            ),
            value::reference_schema(),
            span,
        )?;
        body.push(saved);
        let key = self.expr(&property.expr)?;
        if self.emission_known_throw() {
            owned.extend(key.owned_roots);
            let (input, access) = self.bind_evaluation(key.expr, key.kind, property.span)?;
            if self.has_root(input) {
                owned.push(input);
            }
            body.extend(self.gate_stmts(access, span));
            return Ok((body, owned, base_slot, key_slot, reference));
        }
        if site.key != Mode::Dynamic {
            let expected = match site.key {
                Mode::Boolean => Kind::Boolean,
                Mode::Number => Kind::Number,
                Mode::String => Kind::String,
                Mode::Null => Kind::Null,
                Mode::Undefined => Kind::Undefined,
                Mode::Object => Kind::Object,
                Mode::Dynamic => unreachable!(),
            };
            if key.kind != expected {
                return Err(self.error(
                    property.span,
                    "sealed primitive key fact does not match emission",
                ));
            }
        }
        owned.extend(key.owned_roots);
        let (input, mut access) = self.bind_evaluation(key.expr, key.kind, property.span)?;
        if self.has_root(input) {
            owned.push(input);
        }
        let mut ready = Vec::new();
        self.append_after_coercible(
            site, base_slot, input, key_slot, reference, span, &mut ready, consumer,
        )?;
        access.extend(self.gate_stmts(ready, span));
        body.extend(self.gate_stmts(access, span));
        Ok((body, owned, base_slot, key_slot, reference))
    }

    #[allow(clippy::too_many_arguments)]
    fn append_after_coercible<F>(
        &mut self,
        site: Site,
        base_slot: u16,
        input: u16,
        key_slot: u16,
        reference: u16,
        span: swc_common::Span,
        ready: &mut Vec<Stmt>,
        consumer: F,
    ) -> Result<(), ParseError>
    where
        F: FnOnce(&mut Self, u16, u16, u16, &mut Vec<Stmt>) -> Result<(), ParseError>,
    {
        // Key evaluation already ran. A proved key throw wins; do not convert,
        // build the reference, or lower this consumer.
        if self.emission_known_throw() {
            return Ok(());
        }
        match site.receiver {
            Receiver::Null | Receiver::Undefined => {
                // CheckObjectCoercible before ToString and before the consumer.
                // The throw edge is the post-key, pre-RHS fact state.
                self.note_emission_throw();
                self.append_type_error(span, ready)
            }
            Receiver::Dynamic => {
                self.note_throw_edge();
                self.primitive_key.conditional_throw.set(true);
                let mut continuation = Vec::new();
                self.append_member_continuation(
                    site,
                    base_slot,
                    input,
                    key_slot,
                    reference,
                    span,
                    &mut continuation,
                    consumer,
                )?;
                let line = self.line(span);
                let mut throw = Vec::new();
                self.append_type_error(span, &mut throw)?;
                let mut object_arm = vec![super::assert_stmt(
                    Self::tag_eq(base_slot, value::OBJECT),
                    line,
                )];
                object_arm.extend(continuation);
                ready.push(Stmt::IfElse {
                    condition: Self::nullish_tag(base_slot),
                    then_branch: throw,
                    else_branch: object_arm,
                    line,
                });
                Ok(())
            }
            Receiver::Object | Receiver::Function => self.append_member_continuation(
                site, base_slot, input, key_slot, reference, span, ready, consumer,
            ),
            // A sealed Array with a proved String key reads through the real
            // Reference: the runtime Get owns the array index/`length` virtual
            // rows and then walks the actual prototype pair, so this arm adds
            // no receiver test and no array-specific consumer.
            Receiver::Array if site.key == Mode::String => self.append_member_continuation(
                site, base_slot, input, key_slot, reference, span, ready, consumer,
            ),
            Receiver::Array | Receiver::Boolean | Receiver::Number | Receiver::String => Err(self
                .error(
                    span,
                    "primitive member receiver is outside the object domain",
                )),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn append_member_continuation<F>(
        &mut self,
        site: Site,
        base_slot: u16,
        input: u16,
        key_slot: u16,
        reference: u16,
        span: swc_common::Span,
        ready: &mut Vec<Stmt>,
        consumer: F,
    ) -> Result<(), ParseError>
    where
        F: FnOnce(&mut Self, u16, u16, u16, &mut Vec<Stmt>) -> Result<(), ParseError>,
    {
        self.append_key_conversion(site.key, input, key_slot, span, ready)?;
        if matches!(site.key, Mode::Object | Mode::Dynamic) {
            // The Object or Dynamic key conversion can throw a managed TypeError. Build
            // the prepared Reference and run the role consumer only while
            // that completion is still normal.
            let mut rest = Vec::new();
            rest.push(Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: reference,
                expr: value::reference(
                    Expr::Var(base_slot),
                    value::field(key_slot, "ref_slot"),
                    value::field(key_slot, "generation"),
                    Expr::Bool(self.enclosing_strict),
                ),
                line: self.line(span),
            });
            let carrier = self.active_carrier(span)?;
            let line = self.line(span);
            self.reaffirm_root(carrier, base_slot, span, line, &mut rest)?;
            self.reaffirm_root(carrier, key_slot, span, line, &mut rest)?;
            consumer(self, reference, base_slot, key_slot, &mut rest)?;
            ready.extend(self.gate_stmts(rest, span));
            return Ok(());
        }
        ready.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: reference,
            expr: value::reference(
                Expr::Var(base_slot),
                value::field(key_slot, "ref_slot"),
                value::field(key_slot, "generation"),
                Expr::Bool(self.enclosing_strict),
            ),
            line: self.line(span),
        });
        let carrier = self.active_carrier(span)?;
        let line = self.line(span);
        self.reaffirm_root(carrier, base_slot, span, line, ready)?;
        self.reaffirm_root(carrier, key_slot, span, line, ready)?;
        consumer(self, reference, base_slot, key_slot, ready)
    }

    pub(super) fn lower_primitive_get(
        &mut self,
        member: &ast::MemberExpr,
    ) -> Result<Lowered, ParseError> {
        let span = member.span;
        let (result, init) = self.bind_temp(value::undefined(), span)?;
        let (mut body, mut owned, _, _, _) =
            self.with_prepared_member(member, Role::Get, |lower, reference, _, _, stmts| {
                let carrier = lower.active_carrier(span)?;
                let helper = lower.helper_ref_get.ok_or_else(|| {
                    lower.error(span, "primitive key requires the realm reference helper")
                })?;
                let got = lower.consume_value_result(
                    Self::guest_call(helper, vec![Expr::Var(carrier), Expr::Var(reference)]),
                    span,
                    stmts,
                )?;
                stmts.push(Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: result,
                    expr: Expr::Var(got),
                    line: lower.line(span),
                });
                lower.refresh_root(result, span, stmts)?;
                Ok(())
            })?;
        body.splice(0..0, init);
        owned.push(result);
        reference::release_consumed_roots(
            self,
            &Self::compose_owned(owned),
            result,
            span,
            &mut body,
        )?;
        Ok(Lowered {
            expr: Expr::Block {
                stmts: body,
                expr: Box::new(Expr::Var(result)),
            },
            kind: Kind::Dynamic,
            array_length: None,
            owned_roots: vec![result],
        })
    }

    pub(super) fn put_prepared_slot(
        &mut self,
        member: &ast::MemberExpr,
        value_slot: u16,
        span: swc_common::Span,
        statements: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        let (mut body, owned, _, _, _) =
            self.with_prepared_member(member, Role::Put, |lower, reference, _, _, stmts| {
                let helper = lower
                    .objects
                    .ok_or_else(|| lower.error(span, "ordinary runtime missing"))?
                    .put;
                let carrier = lower.active_carrier(span)?;
                let mut put = Vec::new();
                lower.consume_result(
                    Self::guest_call(
                        helper,
                        vec![
                            Expr::Var(carrier),
                            Expr::Var(reference),
                            Expr::Var(value_slot),
                        ],
                    ),
                    TypeSchema::Null,
                    Expr::Null,
                    |_| value::undefined(),
                    span,
                    &mut put,
                )?;
                stmts.extend(lower.gate_stmts(put, span));
                Ok(())
            })?;
        reference::release_consumed_roots(self, &owned, value_slot, span, &mut body)?;
        statements.extend(body);
        Ok(())
    }

    pub(super) fn lower_primitive_number_put(
        &mut self,
        member: &ast::MemberExpr,
        right: &ast::Expr,
        span: swc_common::Span,
    ) -> Result<Lowered, ParseError> {
        let (out, saved) = self.bind_temp_bare(value::undefined(), span)?;
        let extra_roots = std::cell::RefCell::new(Vec::new());
        let (mut body, mut owned, _, _, _) =
            self.with_prepared_member(member, Role::Put, |lower, reference, _, _, stmts| {
                let rhs = lower.expr(right)?;
                let (rhs_slot, bound) = lower.bind_temp(rhs.expr, right.span())?;
                let mut after = bound;
                let helper = lower
                    .objects
                    .ok_or_else(|| lower.error(span, "ordinary runtime missing"))?
                    .put;
                let carrier = lower.active_carrier(span)?;
                let mut put = Vec::new();
                lower.consume_result(
                    Self::guest_call(
                        helper,
                        vec![
                            Expr::Var(carrier),
                            Expr::Var(reference),
                            Expr::Var(rhs_slot),
                        ],
                    ),
                    TypeSchema::Null,
                    Expr::Null,
                    |_| value::undefined(),
                    span,
                    &mut put,
                )?;
                let mut publish = vec![Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: out,
                    expr: Expr::Var(rhs_slot),
                    line: lower.line(span),
                }];
                lower.refresh_root(out, span, &mut publish)?;
                put.extend(lower.gate_stmts(publish, span));
                after.extend(lower.gate_stmts(put, span));
                stmts.extend(after);
                let mut extra = rhs.owned_roots;
                extra.push(rhs_slot);
                // Returned through the closure's captured cell below.
                extra_roots.borrow_mut().extend(extra);
                Ok(())
            })?;
        body.insert(0, saved);
        owned.extend(extra_roots.into_inner());
        owned.push(out);
        reference::release_consumed_roots(self, &Self::compose_owned(owned), out, span, &mut body)?;
        Ok(Lowered {
            expr: Expr::Block {
                stmts: body,
                expr: Box::new(Expr::Var(out)),
            },
            kind: Kind::Dynamic,
            array_length: None,
            owned_roots: vec![out],
        })
    }

    pub(super) fn prepare_primitive_call(
        &mut self,
        member: &ast::MemberExpr,
        span: swc_common::Span,
    ) -> Result<(u16, u16, Vec<Stmt>, Vec<u16>), ParseError> {
        let (func, init) = self.bind_temp_bare(value::undefined(), span)?;
        let (mut body, owned, base, _, _) =
            self.with_prepared_member(member, Role::Call, |lower, reference, _, _, stmts| {
                let carrier = lower.active_carrier(span)?;
                let helper = lower.helper_ref_get.ok_or_else(|| {
                    lower.error(span, "primitive key requires the realm reference helper")
                })?;
                let got = lower.consume_value_result(
                    Self::guest_call(helper, vec![Expr::Var(carrier), Expr::Var(reference)]),
                    span,
                    stmts,
                )?;
                stmts.push(Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: func,
                    expr: Expr::Var(got),
                    line: lower.line(span),
                });
                lower.refresh_root(func, span, stmts)?;
                Ok(())
            })?;
        body.insert(0, init);
        Ok((func, base, body, owned))
    }

    pub(super) fn lower_primitive_delete(
        &mut self,
        unary: &ast::UnaryExpr,
        member: &ast::MemberExpr,
    ) -> Result<Lowered, ParseError> {
        let span = unary.span;
        let (out, saved) = self.bind_temp_bare(value::boolean(Expr::Bool(false)), span)?;
        let (mut body, mut owned, _, _, _) =
            self.with_prepared_member(member, Role::Delete, |lower, reference, _, _, stmts| {
                let carrier = lower.active_carrier(span)?;
                let helper = lower
                    .objects
                    .ok_or_else(|| lower.error(span, "ordinary runtime missing"))?
                    .delete;
                let produced = lower.consume_result(
                    Self::guest_call(helper, vec![Expr::Var(carrier), Expr::Var(reference)]),
                    TypeSchema::Bool,
                    Expr::Bool(false),
                    |slot| value::boolean(Expr::Var(slot)),
                    span,
                    stmts,
                )?;
                stmts.push(Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: out,
                    expr: Expr::Var(produced),
                    line: lower.line(span),
                });
                lower.refresh_root(out, span, stmts)?;
                Ok(())
            })?;
        body.insert(0, saved);
        owned.push(out);
        reference::release_consumed_roots(self, &Self::compose_owned(owned), out, span, &mut body)?;
        Ok(Lowered {
            expr: Expr::Block {
                stmts: body,
                expr: Box::new(Expr::Var(out)),
            },
            kind: Kind::Boolean,
            array_length: None,
            owned_roots: vec![out],
        })
    }

    pub(super) fn read_primitive_property(
        &mut self,
        member: &ast::MemberExpr,
        kind: Kind,
        owned: &mut Vec<u16>,
        body: &mut Vec<Stmt>,
    ) -> Result<(u16, u16, Vec<Stmt>), ParseError> {
        let role = if self.primitive_key_role(member.span, Role::Compound) {
            Role::Compound
        } else {
            Role::Update
        };
        let span = member.span;
        let (old, init) = if kind == Kind::Number {
            let (slot, stmt) = self.bind_temp_bare(value::number(Expr::Float(0.0)), span)?;
            (slot, vec![stmt])
        } else {
            self.bind_temp(value::undefined(), span)?
        };
        body.extend(init);
        let (mut prepared, extra, _, _, reference) =
            self.with_prepared_member(member, role, |lower, reference, _, _, stmts| {
                let carrier = lower.active_carrier(span)?;
                let helper = lower.helper_ref_get.ok_or_else(|| {
                    lower.error(span, "primitive key requires the realm reference helper")
                })?;
                let got = lower.consume_value_result(
                    Self::guest_call(helper, vec![Expr::Var(carrier), Expr::Var(reference)]),
                    span,
                    stmts,
                )?;
                let value = if kind == Kind::Number {
                    Self::as_number(super::super::prelude::math::checked_number(Expr::Var(got)))
                } else {
                    Expr::Var(got)
                };
                stmts.push(Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: old,
                    expr: value,
                    line: lower.line(span),
                });
                if kind != Kind::Number {
                    lower.refresh_root(old, span, stmts)?;
                }
                Ok(())
            })?;
        owned.extend(extra);
        if self.has_root(old) {
            owned.push(old);
        }
        body.append(&mut prepared);
        Ok((old, reference, Vec::new()))
    }

    pub(super) fn lower_primitive_in(
        &mut self,
        binary: &ast::BinExpr,
    ) -> Result<Lowered, ParseError> {
        let span = binary.span;
        let site = self.demand_site(span, Role::In)?;
        let left = self.expr(&binary.left)?;
        let mut owned = left.owned_roots;
        let (input, mut body) = self.bind_evaluation(left.expr, left.kind, binary.left.span())?;
        if self.has_root(input) {
            owned.push(input);
        }
        let (key_slot, init) = self.bind_temp(value::undefined(), span)?;
        body.extend(init);
        owned.push(key_slot);
        let (out, saved) = self.bind_temp_bare(value::undefined(), span)?;
        body.push(saved);
        let right = self.expr(&binary.right)?;
        owned.extend(right.owned_roots);
        let (object, saved) = self.bind_temp(right.expr, binary.right.span())?;
        let mut after_left = saved;
        owned.push(object);
        let mut after_both = Vec::new();
        self.emit_in_after_object(site, input, key_slot, object, out, span, &mut after_both)?;
        after_left.extend(self.gate_stmts(after_both, span));
        body.extend(self.gate_stmts(after_left, span));
        reference::release_consumed_roots(self, &owned, out, span, &mut body)?;
        Ok(Lowered {
            expr: Expr::Block {
                stmts: body,
                expr: Box::new(Expr::Var(out)),
            },
            kind: Kind::Boolean,
            array_length: None,
            owned_roots: vec![out],
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_in_after_object(
        &mut self,
        site: Site,
        input: u16,
        key_slot: u16,
        object: u16,
        out: u16,
        span: swc_common::Span,
        stmts: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        let line = self.line(span);
        let mut convert = Vec::new();
        self.append_key_conversion(site.key, input, key_slot, span, &mut convert)?;
        let carrier = self.active_carrier(span)?;
        let helper = self
            .objects
            .ok_or_else(|| self.error(span, "ordinary runtime missing"))?
            .has;
        let got = Self::guest_call(
            helper,
            vec![Expr::Var(carrier), Expr::Var(object), Expr::Var(key_slot)],
        );
        let produced = super::super::prelude::object::result(
            value::value_result_schema(),
            true,
            value::boolean(got),
            self.ir_span(span),
        );
        // The HasProperty consumer runs only while the key conversion
        // completion is still normal. Object and Dynamic keys gate it
        // explicitly; the other primitive arms keep their statement order.
        let mut check = Vec::new();
        let value = self.consume_value_result(produced, span, &mut check)?;
        check.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: out,
            expr: Expr::Var(value),
            line,
        });
        self.refresh_root(out, span, &mut check)?;
        let gated = matches!(site.key, Mode::Object | Mode::Dynamic);
        if object_like(site.receiver) {
            if gated {
                stmts.extend(convert);
                stmts.extend(self.gate_stmts(check, span));
            } else {
                convert.extend(check);
                stmts.extend(convert);
            }
            return Ok(());
        }
        if nullish(site.receiver)
            || matches!(
                site.receiver,
                Receiver::Boolean | Receiver::Number | Receiver::String
            )
        {
            return self.append_type_error(span, stmts);
        }
        let mut throw = Vec::new();
        self.append_type_error(span, &mut throw)?;
        let then_branch = if gated {
            let mut branch = convert;
            branch.extend(self.gate_stmts(check, span));
            branch
        } else {
            let mut branch = convert;
            branch.extend(check);
            branch
        };
        stmts.push(Stmt::IfElse {
            condition: Self::tag_eq(object, value::OBJECT),
            then_branch,
            else_branch: throw,
            line,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Entry, Mode, Receiver, Role, Site, register};
    use std::collections::HashMap;

    fn site(role: Role, key: Mode) -> Site {
        Site {
            role,
            key,
            receiver: Receiver::Object,
            scope: 1,
            strict: false,
        }
    }

    #[test]
    fn primitive_key151_seal_probe_and_coverage() {
        let mut sites = HashMap::new();
        register(
            &mut sites,
            false,
            true,
            (1, 2),
            site(Role::Get, Mode::Number),
        )
        .unwrap();
        assert!(sites.is_empty(), "entry probe publishes nothing");
        register(
            &mut sites,
            false,
            false,
            (1, 2),
            site(Role::Get, Mode::Dynamic),
        )
        .unwrap();
        register(
            &mut sites,
            true,
            true,
            (1, 2),
            site(Role::Get, Mode::Number),
        )
        .unwrap();
        assert!(
            register(
                &mut sites,
                true,
                true,
                (1, 2),
                site(Role::Put, Mode::Number)
            )
            .is_err()
        );
        assert!(
            register(
                &mut sites,
                true,
                false,
                (9, 9),
                site(Role::Get, Mode::String)
            )
            .is_err()
        );
        let mut other = HashMap::new();
        register(
            &mut other,
            false,
            false,
            (3, 4),
            site(Role::Call, Mode::Boolean),
        )
        .unwrap();
        assert!(
            register(
                &mut other,
                false,
                false,
                (3, 4),
                site(Role::Delete, Mode::Boolean)
            )
            .is_err()
        );
        let mut scope = site(Role::In, Mode::Null);
        scope.scope = 7;
        register(&mut other, false, false, (5, 6), site(Role::In, Mode::Null)).unwrap();
        assert!(register(&mut other, false, false, (5, 6), scope).is_err());
    }

    fn with_features(
        source: &str,
        body: impl FnOnce(&mut super::super::Lowerer<'_>, Result<(), vm::ParseError>),
    ) {
        let source_id = 0;
        let (script, base) = crate::javascript::parser::parse_script(source, source_id).unwrap();
        let mut lower = super::super::Lowerer {
            globals: crate::javascript::global_binding::Plan::default(),
            global_helpers: None,
            global_env: None,
            source,
            source_id,
            base,
            next_slot: 0,
            bindings: std::collections::HashMap::new(),
            array_lengths: std::collections::HashMap::new(),
            array_length_ids: std::collections::HashMap::new(),
            array_names: std::collections::HashSet::new(),
            source_bindings: Vec::new(),
            heap: None,
            intrinsics: None,
            objects: None,
            string_length_sites: std::cell::RefCell::new(std::collections::HashSet::new()),
            string_length_sealed: std::cell::Cell::new(false),
            helper_string_length: None,
            string_concat_sites: std::cell::RefCell::new(std::collections::HashMap::new()),
            string_concat_sealed: std::cell::Cell::new(false),
            helper_string_concat: None,
            helper_fixed_scalar_string: None,
            primitive_number: super::super::primitive_number::State::default(),
            primitive_addition: super::super::primitive_addition::State::default(),
            mutation: super::super::primitive_assignment::State::default(),
            abstract_equality: super::super::abstract_equality::State::default(),
            relational_sites: std::cell::RefCell::new(std::collections::HashMap::new()),
            relational_sealed: std::cell::Cell::new(false),
            helper_string_compare: None,
            number_key_sites: std::cell::RefCell::new(std::collections::HashSet::new()),
            number_key_sealed: std::cell::Cell::new(false),
            number_string_entries: None,
            primitive_key: super::State::default(),
            init_cells: 0,
            math_used: std::cell::Cell::new(false),
            numeric_globals: std::cell::Cell::new(0),
            uri_used: std::cell::Cell::new(0),
            json_used: std::cell::Cell::new(false),
            number_namespace_used: std::cell::Cell::new(false),
                number_to_string_used: std::cell::Cell::new(false),
                function_name_used: std::cell::Cell::new(false),
                function_to_string_used: std::cell::Cell::new(false),
function_prototype_used: std::cell::Cell::new(false),
            primitive_constructor_read: std::cell::Cell::new(false),
construct_used: std::cell::Cell::new(false),
helper_construct: None,
            helper_construct_check: None,
                string_replace_all_used: std::cell::Cell::new(false),
                string_search_used: std::cell::Cell::new(false),
                string_match_used: std::cell::Cell::new(false),
            string_namespace_used: std::cell::Cell::new(false),
            array_namespace_used: std::cell::Cell::new(false),
            array_constructor_used: std::cell::Cell::new(false),
            array_buffer_used: std::cell::Cell::new(false),
            typed_array_elements: std::cell::Cell::new(0),
            boolean_used: std::cell::Cell::new(false),
            range_error_used: std::cell::Cell::new(false),
            optional_errors_used: std::cell::Cell::new(0),
            utf16_bridge: None,
            uri_call: None,
            init_nodes: 0,
            string_eq: None,
            enclosing_strict: false,
            function_this: None,
            function_args: None,
            arguments_array_from_values: None,
            arguments_string_key: None,
            arguments_sync: None,
            arguments_object: None,
            carrier_override: None,
            frame_override: None,
            root_scope: None,
            helper_decode: None,
            helper_ref_get: None,
            helper_dispatch: None,
            helper_int_identity: None,
            function_code_ids: std::collections::HashMap::new(),
            collected_functions: Vec::new(),
            graph: super::super::scope::build(&script, source, source_id, base).unwrap(),
            active_scope: super::super::scope::ScopeId(0),
            iteration: super::super::iteration::IterationState::default(),
            next_target: 1,
            catch_overlay: std::cell::Cell::new(None),
            needs_ordinary_realm: std::cell::Cell::new(false),
            pending_normal: None,
            pending_completion: None,
            pending_payload: None,
            uncaught_host: None,
            binding_slots: std::collections::HashMap::new(),
            binding_kinds: std::collections::HashMap::new(),
            type_names: None,
            utf16_inits: super::super::utf16_init::Utf16InitPool::default(),
        };
        lower.active_scope = lower.graph.script;
        super::super::collect_source_functions(&script, &mut lower, false).unwrap();
        let result = lower.collect_function_feature_sites();
        body(&mut lower, result);
    }

    #[test]
    fn primitive_key151_catch_overlay_effective_scope() {
        let source = "function f(){var o={};try{throw 1.5;}catch(key){return o[key];}}";
        with_features(source, |lower, result| {
            assert!(result.is_ok(), "{result:?}");
            let parent = lower.graph.script;
            assert_eq!(lower.active_scope, parent, "parent scope restored");
            assert!(lower.catch_overlay.get().is_none(), "overlay restored");
            assert_eq!(lower.graph.catch_scopes.len(), 1);
            assert_eq!(lower.graph.function_scopes.len(), 1);
            let catch_scope = *lower.graph.catch_scopes.values().next().unwrap();
            let function_scope = *lower.graph.function_scopes.values().next().unwrap();
            assert_ne!(catch_scope, function_scope);
            let sites = lower.primitive_key.sites.borrow();
            assert_eq!(sites.len(), 1, "catch computed key must be sealed");
            let site = sites.values().next().unwrap();
            eprintln!(
                "PKEY151_CATCH_SEAL recorded={} catch={} function={} overlay_none={} parent_restored={}",
                site.scope,
                catch_scope.0,
                function_scope.0,
                lower.catch_overlay.get().is_none(),
                lower.active_scope == parent
            );
            assert_eq!(
                site.scope, catch_scope.0,
                "recorded scope must be the catch overlay, not function active_scope"
            );
            assert_ne!(site.scope, function_scope.0);
            assert_eq!(site.role, Role::Get);
            assert_eq!(
                site.prepared.expect("prepared proof").key,
                Mode::Dynamic
            );
            assert!(!site.strict);
        });
    }

    fn span_of(key: (u32, u32)) -> swc_common::Span {
        swc_common::Span::new(swc_common::BytePos(key.0), swc_common::BytePos(key.1))
    }

    #[test]
    fn primitive_key151_catch_overlay_restored_after_object_key_admission() {
        let source = "function f(){var o={};try{throw 1;}catch(key){return o[{}];}}";
        with_features(source, |lower, result| {
            assert!(result.is_ok(), "{result:?}");
            assert_eq!(lower.active_scope, lower.graph.script, "parent scope restored");
            assert!(lower.catch_overlay.get().is_none(), "overlay restored");
            let catch_scope = *lower.graph.catch_scopes.values().next().unwrap();
            let sites = lower.primitive_key.sites.borrow();
            assert_eq!(sites.len(), 1);
            let entry = sites.values().next().unwrap();
            assert_eq!(entry.scope, catch_scope.0);
            assert_eq!(entry.prepared.expect("prepared object-key proof").key, Mode::Object);
        });
    }

    #[test]
    fn primitive_key151_catch_overlay_restored_on_validation_error() {
        let source = "function f(){var o={};try{throw 1;}catch(key){return o[[]];}}";
        with_features(source, |lower, result| {
            let error = result.expect_err("array catch key must refuse");
            let text = format!("{error:?}");
            assert!(
                text.contains("primitive computed keys reject object ToPrimitive"),
                "{text}"
            );
            assert_eq!(
                lower.active_scope, lower.graph.script,
                "parent scope restored"
            );
            assert!(lower.catch_overlay.get().is_none(), "overlay restored");
            assert!(lower.primitive_key.sites.borrow().is_empty());
        });
    }

    #[test]
    fn primitive_key151_no_overlay_records_active_scope() {
        let source = "function f(){var key=\"a\";var o={\"a\":7};return o[key];}";
        with_features(source, |lower, result| {
            assert!(result.is_ok(), "{result:?}");
            assert!(lower.catch_overlay.get().is_none());
            assert!(lower.graph.catch_scopes.is_empty());
            let function_scope = *lower.graph.function_scopes.values().next().unwrap();
            let site = *lower.primitive_key.sites.borrow().values().next().unwrap();
            assert_eq!(site.scope, function_scope.0);
            assert_ne!(site.scope, lower.graph.script.0);
            assert_eq!(site.prepared.expect("prepared proof").key, Mode::String);
        });
    }

    #[test]
    fn primitive_key151_demand_rejects_wrong_scope_role_strictness_unseen() {
        let source = "function f(){var o={};try{throw 1.5;}catch(key){return o[key];}}";
        with_features(source, |lower, result| {
            assert!(result.is_ok(), "{result:?}");
            let catch_scope = *lower.graph.catch_scopes.values().next().unwrap();
            let function_scope = *lower.graph.function_scopes.values().next().unwrap();
            let (key, site) = {
                let sites = lower.primitive_key.sites.borrow();
                let (key, site) = sites.iter().next().unwrap();
                (*key, *site)
            };
            let span = span_of(key);
            lower.primitive_key.sealed.set(true);
            lower.enclosing_strict = site.strict;
            lower.active_scope = function_scope;
            let wrong_scope = lower.demand_site(span, site.role).expect_err("wrong scope");
            assert!(
                format!("{wrong_scope:?}")
                    .contains("primitive key activation does not match emission"),
                "{wrong_scope:?}"
            );
            lower.active_scope = catch_scope;
            let wrong_role = lower.demand_site(span, Role::Put).expect_err("wrong role");
            assert!(
                format!("{wrong_role:?}").contains("primitive key site missing at emission"),
                "{wrong_role:?}"
            );
            lower.enclosing_strict = !site.strict;
            let wrong_strict = lower
                .demand_site(span, site.role)
                .expect_err("wrong strictness");
            assert!(
                format!("{wrong_strict:?}")
                    .contains("primitive key activation does not match emission"),
                "{wrong_strict:?}"
            );
            lower.enclosing_strict = site.strict;
            let missing = lower
                .demand_site(span_of((1, 2)), site.role)
                .expect_err("unseen span");
            assert!(
                format!("{missing:?}").contains("primitive key site missing at emission"),
                "{missing:?}"
            );
            let matched = lower
                .demand_site(span, site.role)
                .expect("matching emission");
            assert_eq!(matched.scope, catch_scope.0);
            assert_eq!(matched.role, site.role);
            assert_eq!(matched.strict, site.strict);
        });
    }

    /// The demand query must read the registry's surviving proof state, not an
    /// admission-time flag: metadata-only records and retracted proofs demand
    /// nothing, a live prepared proof demands the realm.
    #[test]
    fn primitive_key151_realm_demand_counts_only_surviving_proofs() {
        let state = super::State::default();
        assert!(
            !state.publishes_realm_demand(),
            "an empty registry demands no realm"
        );
        state.sites.borrow_mut().insert(
            (1, 2),
            Entry {
                role: Role::Get,
                scope: 1,
                strict: false,
                prepared: None,
                array_string: false,
            },
        );
        assert!(
            !state.publishes_realm_demand(),
            "an observation-only metadata record publishes no demand"
        );
        state.sites.borrow_mut().get_mut(&(1, 2)).unwrap().prepared =
            Some(site(Role::Get, Mode::String));
        assert!(
            state.publishes_realm_demand(),
            "a surviving prepared proof demands the realm"
        );
        state.sites.borrow_mut().get_mut(&(1, 2)).unwrap().prepared = None;
        assert!(
            !state.publishes_realm_demand(),
            "a retracted proof withdraws its demand"
        );
    }

    /// Probe and dead-capability admission stay capability-only: they admit the
    /// same Route for later checks but publish no proof and no demand. A live
    /// admission of the same shape still publishes and demands.
    #[test]
    fn primitive_key151_probe_and_dead_admission_publish_no_demand() {
        let source = "var a=[7];var key='0';a[key];";
        with_features(source, |lower, result| {
            assert!(result.is_ok(), "{result:?}");
            let (parsed, _) = crate::javascript::parser::parse_script(source, 0).unwrap();
            let member = match parsed.body.last() {
                Some(super::ast::Stmt::Expr(expr)) => match &*expr.expr {
                    super::ast::Expr::Member(member) => member,
                    _ => panic!("computed member statement"),
                },
                _ => panic!("computed member statement"),
            };
            let probe = lower
                .probe_primitive_key(|| {
                    lower.admit_primitive_member(
                        member,
                        super::Kind::Array,
                        super::Kind::String,
                        Role::Get,
                    )
                })
                .unwrap();
            assert!(probe, "the probe still admits the Array/String route");
            assert!(
                lower.primitive_key.sites.borrow().is_empty(),
                "a probe walk registers nothing"
            );
            assert!(!lower.needs_ordinary_realm.get(), "no cell widening");
            assert!(!lower.ordinary_realm_demand(), "no probe demand");

            let dead = lower
                .probe_dead_capability(|| {
                    lower.admit_primitive_member(
                        member,
                        super::Kind::Array,
                        super::Kind::String,
                        Role::Get,
                    )
                })
                .unwrap();
            assert!(dead, "dead text is still admitted for capability");
            assert!(
                lower.primitive_key.sites.borrow().is_empty(),
                "a dead-capability walk registers nothing"
            );
            assert!(!lower.needs_ordinary_realm.get(), "no cell widening");
            assert!(!lower.ordinary_realm_demand(), "no dead demand");

            let live = lower
                .admit_primitive_member(
                    member,
                    super::Kind::Array,
                    super::Kind::String,
                    Role::Get,
                )
                .unwrap();
            assert!(live, "live admission still seals the proof");
            assert_eq!(lower.primitive_key.sites.borrow().len(), 1);
            assert!(
                lower.ordinary_realm_demand(),
                "the surviving proof demands the realm it consumes"
            );
        });
    }

    /// A dead text read after an unconditional throw must not publish a proof
    /// (or with it, helper cost) even though its admission still succeeds.
    #[test]
    fn primitive_key151_dead_read_after_throw_publishes_no_proof() {
        let source = "function f(){var a=[7];var key='0';throw 7;a[key];}";
        with_features(source, |lower, result| {
            assert!(result.is_ok(), "{result:?}");
            assert!(
                lower.primitive_key.sites.borrow().is_empty(),
                "the dead read publishes no prepared proof"
            );
            assert!(!lower.needs_ordinary_realm.get());
            assert!(!lower.ordinary_realm_demand());
        });
    }

    /// Disarm is monotone for live observations only. Probe and dead walks
    /// leave the surviving proof untouched; the real observation retracts both
    /// the Array/String proof and the demand derived from it.
    #[test]
    fn primitive_key151_disarm_withdraws_demand_only_when_live() {
        let source = "function f(){var a=[7];var key='0';return a[key];}";
        with_features(source, |lower, result| {
            assert!(result.is_ok(), "{result:?}");
            let (span, scope) = {
                let sites = lower.primitive_key.sites.borrow();
                assert_eq!(sites.len(), 1);
                let (key, entry) = sites.iter().next().unwrap();
                assert!(entry.prepared.is_some(), "live admission seals a proof");
                assert!(entry.array_string);
                (span_of(*key), entry.scope)
            };
            let function_scope = *lower.graph.function_scopes.values().next().unwrap();
            assert_eq!(scope, function_scope.0, "the proof belongs to f's activation");
            assert!(lower.ordinary_realm_demand());
            lower.active_scope = function_scope;
            lower
                .probe_primitive_key(|| {
                    lower.disarm_member_route(span, Role::Get);
                    Ok(())
                })
                .unwrap();
            assert!(
                lower
                    .primitive_key
                    .sites
                    .borrow()
                    .values()
                    .next()
                    .unwrap()
                    .prepared
                    .is_some(),
                "a probe walk cannot disarm"
            );
            lower
                .probe_dead_capability(|| {
                    lower.disarm_member_route(span, Role::Get);
                    Ok(())
                })
                .unwrap();
            assert!(
                lower
                    .primitive_key
                    .sites
                    .borrow()
                    .values()
                    .next()
                    .unwrap()
                    .prepared
                    .is_some(),
                "dead capability cannot disarm"
            );
            lower.disarm_member_route(span, Role::Get);
            let sites = lower.primitive_key.sites.borrow();
            let entry = sites.values().next().unwrap();
            assert_eq!(sites.len(), 1, "the identity record survives");
            assert!(entry.prepared.is_none(), "the Array proof is retracted");
            assert!(!entry.array_string);
            drop(sites);
            assert!(!lower.ordinary_realm_demand(), "a disarmed proof demands nothing");
        });
    }

    /// The aggregate is an OR: an independent producer that already published
    /// its own demand is never dropped by the primitive-key query, even when
    /// the registry holds no proof at all.
    #[test]
    fn primitive_key151_independent_demand_cell_is_never_dropped() {
        let source = "var a=[7];var key='0';a[key];";
        with_features(source, |lower, result| {
            assert!(result.is_ok(), "{result:?}");
            assert!(lower.primitive_key.sites.borrow().is_empty());
            assert!(!lower.ordinary_realm_demand());
            // An independent producer (Object literal, builtin, Math/URI/JSON,
            // throw helper, member write, ...) publishes this shared cell.
            lower.needs_ordinary_realm.set(true);
            assert!(
                lower.ordinary_realm_demand(),
                "an independent producer cell demands the ordinary realm"
            );
        });
    }

    /// Exercise the real emission entry, not just feature validation: a whole
    /// dead known-Array/String read still compiles while the discarded text
    /// materializes no Reference prep and no ordinary realm.
    fn lower_emission(source: &str) -> Result<vm::FrontendIr, vm::ParseError> {
        let (script, base) = crate::javascript::parser::parse_script(source, 0).unwrap();
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = vm::SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::compiler::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        super::super::lower_with_realm(&script, source, 0, base, &context).map(|(ir, _owned)| ir)
    }

    fn lacks_ordinary(ir: &vm::FrontendIr) -> bool {
        ir.functions
            .iter()
            .all(|function| function.name != "__compat_es5_object_proto")
    }

    #[test]
    fn primitive_key151_whole_dead_array_string_read_compiles_capability_only() {
        // Two real routes, never one shared expectation: the variable String
        // key is capability-only dead text (no proof, no realm), while the
        // original literal `a['0']` control keeps the pre-existing independent
        // demand from reference.rs's non-dense-only rule (`open_computed_key`
        // rejects a Lit::Str key, so no primitive-key proof exists there).
        for (source, expected_ordinary) in [
            ("var a=[7];var key='0';throw 7;a[key];", false),
            ("var a=[7];var key='0';throw 7;a['0'];", true),
        ] {
            let ir = lower_emission(source).unwrap_or_else(|error| {
                panic!("dead whole text must still compile {source:?}: {error:?}")
            });
            let ordinary = !lacks_ordinary(&ir);
            assert_eq!(
                ordinary, expected_ordinary,
                "ordinary realm must follow its real demand source {source:?}"
            );
            eprintln!(
                "PKEY151_DEAD_READ_CAPABILITY_ONLY source={source:?} functions={} ordinary={ordinary}",
                ir.functions.len()
            );
        }
    }

    /// The call role has the same discarded boundary as reads: the source
    /// compiles with capability validation, publishes no primitive-key proof
    /// and adds no realm demand of its own. The callee's Array/String path
    /// never materializes a Reference or helper in the discarded run.
    #[test]
    fn primitive_key151_whole_dead_array_string_call_compiles_capability_only() {
        let source = "var a=[7];var key='hasOwnProperty';throw 7;a[key]('0');";
        let ir = lower_emission(source).expect("whole dead call must compile");
        assert!(
            lacks_ordinary(&ir),
            "a discarded call must not publish its own realm demand"
        );
        eprintln!(
            "PKEY151_DEAD_CALL_CAPABILITY_ONLY functions={}",
            ir.functions.len()
        );
    }

    /// The independent Object-literal producer is never cleared by the dead
    /// read boundary: `o[key]` compiles capability-only while the earlier
    /// ordinary demand survives.
    #[test]
    fn primitive_key151_whole_dead_object_string_read_keeps_independent_demand() {
        let source = "var o={a:1};var key='a';throw 7;o[key];";
        let ir = lower_emission(source).expect("whole dead object read must compile");
        assert!(
            !lacks_ordinary(&ir),
            "the surviving Object-literal demand must stay"
        );
    }

    /// Whole-dead representatives of every already-admitted Reference role.
    /// Each source keeps the live receiver/key/RHS/args admission and compiles
    /// without an executable prepared Reference.
    #[test]
    fn primitive_key151_whole_dead_reference_roles_compile() {
        for source in [
            "var o={a:1};var key='a';throw 7;o[key];",
            "function f(){return 1;}var key='call';throw 7;f[key];",
            "var a=[7];var key='hasOwnProperty';throw 7;a[key]('0');",
            "var o={a:1};var key='a';throw 7;o[key]=5;",
            "var o={a:1};var key='a';throw 7;delete o[key];",
            "var o={a:1};var n=1;throw 7;n in o;",
            "var o={a:1};var key='a';throw 7;o[key]+=1;",
            "var o={a:1};var key='a';throw 7;o[key]++;",
        ] {
            lower_emission(source).unwrap_or_else(|error| {
                panic!("discarded Reference role must compile {source:?}: {error:?}")
            });
        }
    }

    /// The capability boundary never widens the admitted key/receiver domain:
    /// the original Object ToPrimitive and dense-Array refusals still fire in
    /// whole-dead text.
    #[test]
    fn primitive_key151_whole_dead_reference_roles_keep_refusals() {
        for source in [
            "var o={a:1};var key='a';throw 7;o[[]];",
            "var a=[7];var key='0';throw 7;a[{}];",
        ] {
            lower_emission(source).expect_err("discarded text keeps the live refusal");
        }
    }

    /// The real shared statement/for-header write owner (`lower_expr_value` ->
    /// `lower_member_write_stmt`) takes the existing capability boundary
    /// before the legacy identifier/string-literal key gate. The originally
    /// failing bare Put, its parenthesized-base peer and the same owner's `for`
    /// init/update compile; the legacy dense numeric Array Put keeps its old
    /// route and the two original Array String/Object key writes still refuse.
    #[test]
    fn primitive_key151_whole_dead_statement_put_owner_routes_capability() {
        for source in [
            "var o={a:1};var key='a';throw 7;o[key]=5;",
            "var o={a:1};var key='a';throw 7;(o)[key]=5;",
            "var o={a:1};var key='a';throw 7;for(o[key]=0;false;o[key]=1){}",
            "var a=[7];throw 7;a[0]=5;",
        ] {
            lower_emission(source).unwrap_or_else(|error| {
                panic!("discarded statement Put must compile {source:?}: {error:?}")
            });
        }
        for source in [
            "var a=[7];var key='x';throw 7;a[key]=5;",
            "var a=[7];var o={};throw 7;a[o]=5;",
        ] {
            lower_emission(source).expect_err("the dense Array key refusal must stay");
        }
    }

    /// A proved nullish test publishes its own prepared Null proof and makes
    /// both arms capability text, so the dead Array arm publishes no proof and
    /// the surviving nullish proof keeps the aggregate demand. The original
    /// literal-key source stays as a legacy control: `null['x']` never opens a
    /// primitive-key site (`open_computed_key` rejects a Lit::Str key), so its
    /// realm demand is the pre-existing independent cell and the then-branch
    /// Array read is prepared on its own route, never claimed statically dead.
    #[test]
    fn primitive_key151_abrupt_operand_publishes_no_proof_but_keeps_demand() {
        let source = "function f(){var a=[7];var key='0';var n='x';var z=null[n]?a[key]:0;return z;}";
        with_features(source, |lower, result| {
            assert!(result.is_ok(), "{result:?}");
            let sites = lower.primitive_key.sites.borrow();
            assert_eq!(
                sites.len(),
                1,
                "only the admitted nullish receiver is sealed"
            );
            let entry = sites.values().next().unwrap();
            let prepared = entry.prepared.expect("nullish proof");
            assert_eq!(prepared.receiver, Receiver::Null);
            assert_eq!(prepared.key, Mode::String);
            assert!(!entry.array_string, "the dead Array arm is not prepared");
            drop(sites);
            assert!(
                lower.ordinary_realm_demand(),
                "the surviving prepared nullish proof demands its realm"
            );
        });
        lower_emission(source).expect("the abrupt operand still compiles");

        let legacy = "function f(){var a=[7];var key='0';var z=null['x']?a[key]:0;return z;}";
        with_features(legacy, |lower, result| {
            assert!(result.is_ok(), "{result:?}");
            let sites = lower.primitive_key.sites.borrow();
            assert_eq!(sites.len(), 1, "the then-branch Array read is prepared");
            let entry = sites.values().next().unwrap();
            assert_eq!(
                entry.prepared.expect("Array proof").receiver,
                Receiver::Array,
                "a literal-key nullish read proves no Null receiver"
            );
            assert!(
                entry.array_string,
                "the Array/String proof stays on its narrow route"
            );
            drop(sites);
            assert!(
                lower.ordinary_realm_demand(),
                "the legacy independent realm cell survives untouched"
            );
        });
        lower_emission(legacy).expect("the legacy literal source still compiles");
    }

    /// The capability isolate restores the dead flag on error: an admitted
    /// nullish test registers its prepared Null proof first, then the refused
    /// object/Array key in the dead arm still errors and leaves that proof in
    /// place. The original literal-key source stays as the legacy refusal
    /// control: it opens no prepared site at all, which is its true path. A
    /// whole-dead object subscript on a known Array still refuses the dense
    /// numeric boundary at real emission.
    #[test]
    fn primitive_key151_dead_operand_restores_and_keeps_refusals() {
        let source = "function f(){var o={};var n='x';var z=null[n]?o[[]]:0;return z;}";
        with_features(source, |lower, result| {
            let error = result.expect_err("object key with an Array key must refuse");
            assert!(
                format!("{error:?}").contains("primitive computed keys reject object ToPrimitive"),
                "{error:?}"
            );
            assert!(!lower.in_dead_capability(), "dead flag restored after error");
            let sites = lower.primitive_key.sites.borrow();
            assert_eq!(sites.len(), 1, "the nullish receiver keeps its own site");
            let entry = sites.values().next().unwrap();
            let prepared = entry.prepared.expect("nullish proof");
            assert_eq!(prepared.receiver, Receiver::Null);
        });

        let legacy = "function f(){var o={};var z=null['x']?o[[]]:0;return z;}";
        with_features(legacy, |lower, result| {
            let error = result.expect_err("literal-key object key still refuses");
            let text = format!("{error:?}");
            assert!(
                text.contains("primitive computed keys reject object ToPrimitive"),
                "{text}"
            );
            assert!(!lower.in_dead_capability(), "dead flag restored after error");
            assert!(
                lower.primitive_key.sites.borrow().is_empty(),
                "the literal nullish key opens no prepared site"
            );
        });

        let whole_dead = "var a=[7];var key='0';throw 7;a[{}];";
        let error = lower_emission(whole_dead).expect_err("dead object subscript still refuses");
        assert!(
            format!("{error:?}").contains("P0 dense array indexes must be numeric in this scope"),
            "{error:?}"
        );
    }
}
