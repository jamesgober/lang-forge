//! Fields: what each node kind's labels can hold (LSF2 §11.3).
//!
//! A label applies to every element its labelled part adds directly to the
//! current node; a label on a group or a hidden rule applies to every element
//! added at that level that carries no label from inside (inner labels win).
//! From that rule the forge derives, per node kind, each field's name, the
//! kinds it can hold, and its cardinality — one, optional, or many — computed
//! structurally from where the label occurs: under a repetition it is many,
//! under an optional or in only some alternatives it is optional, and twice in
//! one sequence it is many.
//!
//! The computation is a bottom-up pass over each rule's expressions (children
//! before parents, so no recursion), with hidden rules summarized and the
//! summaries iterated to a fixpoint, since hidden rules can call each other.
//!
//! Each summary lists kinds, so a hostile sketch (a hidden rule over thousands
//! of keywords, referenced from thousands of places) could make the summaries
//! grow as expressions times kinds. Every kind list computed counts against
//! [`BUDGET`]; past it the sketch is refused (`LSF9008`) instead of followed.

use alloc::{boxed::Box, collections::BTreeMap, vec, vec::Vec};

use crate::{
    grammar::{Analysis, Expr, FieldDef, Pratt, Rule, RuleBody, Shape},
    schematic::Fixity,
};

/// The most kind entries the summaries may compute in total, over every
/// round: far above any real grammar (Mox's sketch computes about 30,000), and about
/// 64 MiB of `u16`s at most.
const BUDGET: u64 = 1 << 25;

/// How many elements a field or level holds: at least `min`, at most `max`
/// (2 standing for "two or more").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Card {
    min: u8,
    max: u8,
    kinds: Vec<u16>,
}

impl Card {
    fn one(kind: u16) -> Self {
        Self {
            min: 1,
            max: 1,
            kinds: vec![kind],
        }
    }

    /// Adds `kinds`: one linear merge, not an insertion each.
    fn add_kinds(&mut self, kinds: &[u16]) {
        if kinds.is_empty() {
            return;
        }
        if !kinds.windows(2).all(|w| w[0] < w[1]) {
            // Operator-node lists come in level order.
            let mut sorted = kinds.to_vec();
            sorted.sort_unstable();
            sorted.dedup();
            return self.add_kinds(&sorted);
        }
        if self.kinds.is_empty() {
            self.kinds.extend_from_slice(kinds);
            return;
        }
        let mut merged = Vec::with_capacity(self.kinds.len() + kinds.len());
        let (mut i, mut j) = (0, 0);
        while i < self.kinds.len() && j < kinds.len() {
            let (a, b) = (self.kinds[i], kinds[j]);
            merged.push(a.min(b));
            i += usize::from(a <= b);
            j += usize::from(b <= a);
        }
        merged.extend_from_slice(&self.kinds[i..]);
        merged.extend_from_slice(&kinds[j..]);
        self.kinds = merged;
    }

    /// One after the other.
    fn then(&mut self, other: &Card) {
        self.min = (self.min + other.min).min(2);
        self.max = (self.max + other.max).min(2);
        self.add_kinds(&other.kinds);
    }

    /// One or the other.
    fn or(&mut self, other: &Card) {
        self.min = self.min.min(other.min);
        self.max = self.max.max(other.max);
        self.add_kinds(&other.kinds);
    }
}

/// What a part of a rule adds at its own level.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Summary {
    labelled: BTreeMap<u16, Card>,
    unlabelled: Card,
}

impl Summary {
    fn then(&mut self, other: &Summary) {
        for (l, c) in &other.labelled {
            self.labelled.entry(*l).or_default().then(c);
        }
        self.unlabelled.then(&other.unlabelled);
    }

    fn or(&mut self, other: &Summary) {
        // A label missing from one side is absent there: min 0.
        for (l, c) in self.labelled.iter_mut() {
            match other.labelled.get(l) {
                Some(o) => c.or(o),
                None => c.min = 0,
            }
        }
        for (l, c) in &other.labelled {
            if !self.labelled.contains_key(l) {
                let mut c = c.clone();
                c.min = 0;
                let _ = self.labelled.insert(*l, c);
            }
        }
        self.unlabelled.or(&other.unlabelled);
    }

    fn repeat(&mut self, min_one: bool) {
        let scale = |c: &mut Card| {
            if !min_one {
                c.min = 0;
            }
            if c.max > 0 {
                c.max = 2;
            }
        };
        self.labelled.values_mut().for_each(scale);
        scale(&mut self.unlabelled);
    }

    fn optional(&mut self) {
        self.labelled.values_mut().for_each(|c| c.min = 0);
        self.unlabelled.min = 0;
    }

    /// The entries the summary holds, counted against [`BUDGET`].
    fn size(&self) -> u64 {
        let labelled: usize = self.labelled.values().map(|c| c.kinds.len() + 1).sum();
        (labelled + self.unlabelled.kinds.len()) as u64
    }
}

/// The summaries passed [`BUDGET`].
pub(crate) struct TooLarge;

/// Derives every node kind's fields. `labels` is the number of labels.
///
/// # Errors
///
/// [`TooLarge`] if the work passes [`BUDGET`].
pub(crate) fn derive(
    a: &Analysis<'_>,
    rules: &[Rule],
    pratts: &[Pratt],
    shape: &Shape,
    labels: usize,
) -> Result<crate::grammar::FieldTable, TooLarge> {
    if labels == 0 {
        return Ok(Box::new([]));
    }
    let mut spent: u64 = 0;
    let op_labels = shape.v2.as_ref().map_or([0; 4], |v| v.op_labels);
    let word_set = first_of_word(a);
    let word_kinds: Vec<u16> = if word_set == crate::set::NO_SET {
        Vec::new()
    } else {
        a.sets.members(word_set).map(|k| k as u16).collect()
    };
    // Hidden rules' summaries, iterated to a fixpoint (bounded: counts
    // saturate at 2 and kind sets only grow).
    let mut hidden: Vec<Summary> = vec![Summary::default(); rules.len()];
    let mut summaries: Vec<Summary> = vec![Summary::default(); a.exprs.len()];
    for _round in 0..16 {
        let mut changed = false;
        for (r, rule) in rules.iter().enumerate() {
            let s = rule_summary(
                a,
                rules,
                pratts,
                r,
                &hidden,
                &mut summaries,
                &word_kinds,
                &mut spent,
            )?;
            if rule.node.is_none() && s != hidden[r] {
                hidden[r] = s;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // Fields of each node kind.
    let mut fields: BTreeMap<u16, BTreeMap<u16, Card>> = BTreeMap::new();
    let mut add = |node: u16, s: &Summary| {
        let entry = fields.entry(node).or_default();
        for (l, c) in &s.labelled {
            match entry.get_mut(l) {
                Some(existing) => existing.or(c),
                None => {
                    let _ = entry.insert(*l, c.clone());
                }
            }
        }
    };
    for (r, rule) in rules.iter().enumerate() {
        let Some(node) = rule.node else { continue };
        let s = rule_summary(
            a,
            rules,
            pratts,
            r,
            &hidden,
            &mut summaries,
            &word_kinds,
            &mut spent,
        )?;
        add(node.index(), &s);
        if let RuleBody::Pratt(p) = rule.body {
            let pratt = &pratts[p as usize];
            let operand = expr_summary_cached(&summaries, pratt.operand);
            let op_nodes: Vec<u16> = pratt.levels.iter().map(|l| l.node.index()).collect();
            let mut side = operand.unlabelled.clone();
            side.add_kinds(&op_nodes);
            side.min = 1;
            for level in pratt.levels.iter() {
                let mut s = Summary::default();
                // Labels inside the operand stay on the operand's elements.
                for (l, c) in &operand.labelled {
                    let mut c = c.clone();
                    c.min = 0;
                    let _ = s.labelled.insert(*l, c);
                }
                let mut ops: Vec<u16> = (0..pratt.prefix.len())
                    .filter(|&k| {
                        let at = if level.fixity == Fixity::Prefix {
                            pratt.prefix[k]
                        } else {
                            pratt.after[k]
                        };
                        at != 0 && pratt.levels[usize::from(at) - 1].node == level.node
                    })
                    .map(|k| k as u16)
                    .collect();
                ops.extend(pratt.contextual.iter().map(|c| c.0));
                ops.sort_unstable();
                ops.dedup();
                let op = Card {
                    min: 1,
                    max: 1,
                    kinds: ops,
                };
                let [lhs, op_label, rhs, operand_label] = op_labels;
                match level.fixity {
                    Fixity::Prefix => {
                        let _ = s.labelled.insert(op_label, op);
                        let _ = s.labelled.insert(operand_label, side.clone());
                    }
                    Fixity::Postfix => {
                        let _ = s.labelled.insert(operand_label, side.clone());
                        let _ = s.labelled.insert(op_label, op);
                    }
                    _ => {
                        let _ = s.labelled.insert(lhs, side.clone());
                        let _ = s.labelled.insert(op_label, op);
                        let _ = s.labelled.insert(rhs, side.clone());
                    }
                }
                if let Some(then) = level.then {
                    let t = expr_summary_cached(&summaries, then);
                    s.then(&Summary {
                        labelled: t.labelled.clone(),
                        unlabelled: Card::default(),
                    });
                }
                add(level.node.index(), &s);
            }
        }
    }
    Ok(fields
        .into_iter()
        .map(|(node, labels)| {
            let defs: Box<[FieldDef]> = labels
                .into_iter()
                .map(|(label, c)| FieldDef {
                    label,
                    cardinality: match (c.min, c.max) {
                        (_, 2) => 2,
                        (0, _) => 1,
                        _ => 0,
                    },
                    kinds: c.kinds.into(),
                })
                .collect();
            (node, defs)
        })
        .collect())
}

/// The FIRST set of a `WORD` expression (every keyword and `IDENT`), or an
/// empty set if the grammar has none.
fn first_of_word(a: &Analysis<'_>) -> crate::set::SetId {
    a.exprs
        .iter()
        .position(|e| matches!(e, Expr::Word))
        .map_or(crate::set::NO_SET, |e| a.first[e])
}

fn expr_summary_cached(summaries: &[Summary], e: u32) -> Summary {
    summaries.get(e as usize).cloned().unwrap_or_default()
}

/// The summary of rule `r`'s body, computing every expression's summary on
/// the way (children before parents). Each summary computed counts against
/// `spent`.
#[allow(clippy::too_many_arguments)]
fn rule_summary(
    a: &Analysis<'_>,
    rules: &[Rule],
    pratts: &[Pratt],
    r: usize,
    hidden: &[Summary],
    summaries: &mut [Summary],
    word: &[u16],
    spent: &mut u64,
) -> Result<Summary, TooLarge> {
    for &e in &a.owned[r] {
        let e = e as usize;
        let s = match a.exprs[e] {
            Expr::Token(k) | Expr::Keyword(k) => Summary {
                labelled: BTreeMap::new(),
                unlabelled: Card::one(k),
            },
            Expr::Word => Summary {
                labelled: BTreeMap::new(),
                unlabelled: Card {
                    min: 1,
                    max: 1,
                    kinds: word.to_vec(),
                },
            },
            Expr::Rule(callee) => match rules[callee as usize].node {
                Some(node) => Summary {
                    labelled: BTreeMap::new(),
                    unlabelled: Card::one(node.index()),
                },
                None => hidden[callee as usize].clone(),
            },
            Expr::Seq { start, len } => {
                let mut s = Summary::default();
                for &item in a.children(start, len) {
                    s.then(&summaries[item as usize]);
                }
                s
            }
            Expr::Choice { start, len } => {
                let mut items = a.children(start, len).iter();
                match items.next() {
                    None => Summary::default(),
                    Some(&first) => {
                        let mut s = summaries[first as usize].clone();
                        for &item in items {
                            s.or(&summaries[item as usize]);
                        }
                        s
                    }
                }
            }
            Expr::Repeat { body, min_one, .. } => {
                let mut s = summaries[body as usize].clone();
                s.repeat(min_one);
                s
            }
            Expr::Optional(body) => {
                let mut s = summaries[body as usize].clone();
                s.optional();
                s
            }
            Expr::Label { label, body } => {
                let inner = &summaries[body as usize];
                let mut s = Summary {
                    labelled: inner.labelled.clone(),
                    unlabelled: Card::default(),
                };
                s.labelled.entry(label).or_default().then(&inner.unlabelled);
                s
            }
            Expr::BackRef { body, .. } => summaries[body as usize].clone(),
            Expr::And(_) | Expr::Not(_) | Expr::Eof | Expr::LineStart | Expr::NlBefore => {
                Summary::default()
            }
        };
        *spent += s.size();
        if *spent > BUDGET {
            return Err(TooLarge);
        }
        summaries[e] = s;
    }
    Ok(match rules[r].body {
        RuleBody::Expr(e) => summaries[e as usize].clone(),
        RuleBody::Pratt(p) => {
            // The rule's node holds the operand's elements or one operator
            // node.
            let pratt = &pratts[p as usize];
            let mut s = summaries[pratt.operand as usize].clone();
            let mut ops = Summary::default();
            ops.unlabelled.min = 1;
            ops.unlabelled.max = 1;
            ops.unlabelled.add_kinds(
                &pratt
                    .levels
                    .iter()
                    .map(|l| l.node.index())
                    .collect::<Vec<_>>(),
            );
            s.or(&ops);
            s
        }
    })
}

/// How many children a field holds (LSF2 §11.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Cardinality {
    /// Exactly one child.
    One,
    /// None or one.
    Optional,
    /// Any number (a list field, such as `args:arg (',' args:arg)*`).
    Many,
}

/// A field of a node kind: a label its children carry, the kinds it can
/// hold, and how many (see [`Language::fields`](crate::Language::fields)).
///
/// # Examples
///
/// ```
/// use lang_forge::{Cardinality, Language};
///
/// let lang = Language::from_lsf(
///     "[sketch]\nformat = 2\n[language]\nname = \"x\"\nversion = \"1.0.0\"\n\
///      [rules]\npair = \"key:IDENT '=' value:(NUMBER | IDENT)\"\n",
/// )?;
/// let pair = lang.kind("pair").expect("a rule");
/// let value = lang.fields(pair).find(|f| f.name() == "value").expect("labelled");
/// assert_eq!(value.cardinality(), Cardinality::One);
/// let kinds: Vec<&str> = value.kinds().map(|k| lang.kind_name(k)).collect();
/// assert_eq!(kinds, ["IDENT", "NUMBER"]);
/// assert_eq!(lang.label_name(value.label()), Some("value"));
/// # Ok::<(), lang_forge::Error>(())
/// ```
#[derive(Clone, Copy, Debug)]
pub struct Field<'a> {
    language: &'a crate::Language,
    def: &'a FieldDef,
}

impl<'a> Field<'a> {
    pub(crate) fn new(language: &'a crate::Language, def: &'a FieldDef) -> Self {
        Self { language, def }
    }

    /// The label's id (see [`Language::label_id`](crate::Language::label_id)).
    #[must_use]
    pub fn label(&self) -> u16 {
        self.def.label
    }

    /// The label's name.
    #[must_use]
    pub fn name(&self) -> &'a str {
        self.language.label_name(self.def.label).unwrap_or("")
    }

    /// How many children the field holds.
    #[must_use]
    pub fn cardinality(&self) -> Cardinality {
        match self.def.cardinality {
            0 => Cardinality::One,
            1 => Cardinality::Optional,
            _ => Cardinality::Many,
        }
    }

    /// The kinds the field's children can have, in index order.
    pub fn kinds(&self) -> impl Iterator<Item = crate::Kind> + 'a {
        let language = self.language;
        self.def
            .kinds
            .iter()
            .filter_map(move |&k| language.kind_at(k))
    }
}
