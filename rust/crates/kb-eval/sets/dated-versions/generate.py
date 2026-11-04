#!/usr/bin/env python3
"""Generate the synthetic dated-versions set for RET-4.

Deterministic, Python stdlib only: running this with no arguments rewrites the
corpus, the metadata sidecars, the queries and the qrels to the same bytes
every time. There is no randomness and no network access.

Layout (see README.md):

    corpus/<slug>-v<year>.md          three dated versions per family
    metadata/<slug>-v<year>.json      {"effectiveDate": "...", "authority": ...}
    queries.jsonl                     the runner's query format
    queries.tsv                       qid<TAB>query, for humans
    qrels/test.tsv                    qid 0 <document> <relevance>

The task: every family has one fact that changed in 2023, 2024 and 2025. The
qrel marks only the newest version as relevant, so MRR@10 measures how often a
search puts the current edition first. A lexical or dense retriever sees three
near-identical documents and has no reason to prefer the newest; the recency
boost is the only signal that says which one is current.
"""

from __future__ import annotations

import argparse
import json
import shutil
from dataclasses import dataclass
from pathlib import Path

HERE = Path(__file__).resolve().parent
YEARS = (2023, 2024, 2025)


@dataclass(frozen=True)
class Family:
    slug: str
    title: str
    topic: str
    question: str
    fact: str
    values: tuple[str, str, str]
    authority: float | None


# One row per family. `topic` is the noun phrase the question asks about; the
# three values are the 2023, 2024 and 2025 editions, in that order.
RAW_FAMILIES: tuple[
    tuple[str, str, str, str, str, str, str, str, float | None], ...
] = (
    (
        "remote-work-allowance",
        "Remote Work Allowance Policy",
        "remote work allowance",
        "What is the current monthly remote work allowance?",
        "Monthly remote work allowance",
        "50 EUR",
        "75 EUR",
        "100 EUR",
        0.9,
    ),
    (
        "annual-leave",
        "Annual Leave Policy",
        "annual leave entitlement",
        "How many annual leave days does the current policy give?",
        "Annual leave entitlement",
        "20 days",
        "22 days",
        "25 days",
        0.6,
    ),
    (
        "parental-leave",
        "Parental Leave Policy",
        "parental leave entitlement",
        "What is the current parental leave entitlement?",
        "Paid parental leave",
        "12 weeks",
        "14 weeks",
        "16 weeks",
        0.3,
    ),
    (
        "expense-approval",
        "Expense Approval Policy",
        "expense approval limit",
        "What is the current expense approval limit for a manager?",
        "Manager approval limit",
        "500 EUR",
        "750 EUR",
        "1000 EUR",
        0.9,
    ),
    (
        "home-office-stipend",
        "Home Office Stipend Policy",
        "home office stipend",
        "What is the latest home office stipend?",
        "One-off home office stipend",
        "300 EUR",
        "400 EUR",
        "600 EUR",
        0.6,
    ),
    (
        "travel-class",
        "Business Travel Policy",
        "flight class threshold",
        "At what flight length does the current travel policy allow business class?",
        "Business-class threshold",
        "4 hours",
        "6 hours",
        "8 hours",
        0.3,
    ),
    (
        "learning-budget",
        "Learning and Development Policy",
        "annual learning budget",
        "What is the current annual learning budget per employee?",
        "Annual learning budget",
        "800 EUR",
        "1000 EUR",
        "1500 EUR",
        0.9,
    ),
    (
        "health-insurance",
        "Health Insurance Policy",
        "employer health cover",
        "What percentage of health insurance does the current policy cover?",
        "Employer share of health insurance",
        "70 percent",
        "80 percent",
        "90 percent",
        0.6,
    ),
    (
        "notice-period",
        "Notice Period Policy",
        "notice period",
        "What is the current notice period for resignations?",
        "Employee notice period",
        "30 days",
        "45 days",
        "60 days",
        0.3,
    ),
    (
        "probation",
        "Probation Policy",
        "probation length",
        "How long is the current probation period?",
        "Probation period",
        "6 months",
        "5 months",
        "3 months",
        0.9,
    ),
    (
        "overtime",
        "Overtime Policy",
        "overtime multiplier",
        "What is the current overtime pay multiplier?",
        "Overtime multiplier",
        "1.25x",
        "1.5x",
        "1.75x",
        0.6,
    ),
    (
        "sick-leave",
        "Sick Leave Policy",
        "paid sick leave",
        "How many paid sick leave days does the current policy provide?",
        "Paid sick leave",
        "10 days",
        "12 days",
        "15 days",
        0.3,
    ),
    (
        "relocation",
        "Relocation Policy",
        "relocation package",
        "What is the current relocation package?",
        "Relocation package",
        "3000 EUR",
        "5000 EUR",
        "7500 EUR",
        0.9,
    ),
    (
        "referral-bonus",
        "Employee Referral Policy",
        "referral bonus",
        "What is the latest referral bonus?",
        "Referral bonus",
        "500 EUR",
        "750 EUR",
        "1200 EUR",
        0.6,
    ),
    (
        "meeting-free-days",
        "Meeting Culture Policy",
        "meeting-free days",
        "How many meeting-free days per week does the current policy set?",
        "Meeting-free days per week",
        "1 day",
        "2 days",
        "3 days",
        0.3,
    ),
    (
        "support-response",
        "Support Response Policy",
        "first-response target",
        "What is the current first-response target for standard support?",
        "First-response target",
        "48 hours",
        "24 hours",
        "8 hours",
        0.9,
    ),
    (
        "uptime-sla",
        "Service Level Agreement",
        "uptime SLA",
        "What uptime does the current service level agreement promise?",
        "Monthly uptime",
        "99.5 percent",
        "99.9 percent",
        "99.95 percent",
        0.6,
    ),
    (
        "api-rate-limit",
        "API Rate Limit Policy",
        "API rate limit",
        "What is the current API rate limit per minute?",
        "Requests per minute",
        "100 requests",
        "250 requests",
        "500 requests",
        0.3,
    ),
    (
        "free-tier-storage",
        "Free Tier Policy",
        "free tier storage",
        "How much storage does the latest free tier include?",
        "Free-tier storage",
        "5 GB",
        "10 GB",
        "25 GB",
        0.9,
    ),
    (
        "trial-length",
        "Trial Policy",
        "trial length",
        "How long is the current product trial?",
        "Trial length",
        "14 days",
        "21 days",
        "30 days",
        0.6,
    ),
    (
        "password-rotation",
        "Password Policy",
        "password rotation interval",
        "How often does the current password policy require rotation?",
        "Password rotation",
        "180 days",
        "120 days",
        "90 days",
        0.3,
    ),
    (
        "backup-retention",
        "Backup Policy",
        "backup retention",
        "What is the current backup retention period?",
        "Backup retention",
        "30 days",
        "60 days",
        "90 days",
        0.9,
    ),
    (
        "incident-response",
        "Incident Response Policy",
        "incident acknowledgement target",
        "What is the current incident acknowledgement target?",
        "Acknowledgement target",
        "60 minutes",
        "30 minutes",
        "15 minutes",
        0.6,
    ),
    (
        "release-cadence",
        "Release Policy",
        "release cadence",
        "What is the current release cadence?",
        "Release cadence",
        "every 4 weeks",
        "every 3 weeks",
        "every 2 weeks",
        0.3,
    ),
    (
        "code-review",
        "Code Review Policy",
        "required code review approvals",
        "How many code review approvals does the current policy require?",
        "Required approvals",
        "3 approvals",
        "2 approvals",
        "1 approval",
        0.9,
    ),
    (
        "test-coverage",
        "Testing Policy",
        "test coverage threshold",
        "What test coverage does the current policy require?",
        "Minimum line coverage",
        "70 percent",
        "80 percent",
        "85 percent",
        0.6,
    ),
    (
        "vendor-review",
        "Vendor Review Policy",
        "vendor review interval",
        "How often does the current vendor review policy require reassessment?",
        "Vendor reassessment",
        "every 24 months",
        "every 18 months",
        "every 12 months",
        0.3,
    ),
    (
        "data-retention",
        "Data Retention Policy",
        "customer data retention",
        "What is the current customer data retention period?",
        "Customer data retention",
        "36 months",
        "24 months",
        "18 months",
        0.9,
    ),
    (
        "audit-frequency",
        "Audit Policy",
        "internal audit frequency",
        "How often does the current audit policy require an internal audit?",
        "Internal audit frequency",
        "every 12 months",
        "every 6 months",
        "every 3 months",
        0.6,
    ),
    (
        "training-days",
        "Training Policy",
        "training days",
        "How many training days per year does the current policy provide?",
        "Training days per year",
        "5 days",
        "8 days",
        "10 days",
        0.3,
    ),
    (
        "conference-budget",
        "Conference Budget Policy",
        "conference budget",
        "What is the current conference budget per employee?",
        "Conference budget",
        "1000 EUR",
        "1500 EUR",
        "2500 EUR",
        0.9,
    ),
    (
        "wellness-credit",
        "Wellness Policy",
        "wellness credit",
        "What is the latest wellness credit?",
        "Annual wellness credit",
        "200 EUR",
        "300 EUR",
        "450 EUR",
        0.6,
    ),
    (
        "commute-reimbursement",
        "Commute Policy",
        "commute reimbursement",
        "What is the current monthly commute reimbursement?",
        "Monthly commute reimbursement",
        "80 EUR",
        "120 EUR",
        "150 EUR",
        0.3,
    ),
    (
        "internet-stipend",
        "Internet Stipend Policy",
        "internet stipend",
        "What is the current monthly internet stipend?",
        "Monthly internet stipend",
        "25 EUR",
        "40 EUR",
        "60 EUR",
        0.9,
    ),
    (
        "phone-stipend",
        "Phone Stipend Policy",
        "phone stipend",
        "What is the current monthly phone stipend?",
        "Monthly phone stipend",
        "20 EUR",
        "30 EUR",
        "40 EUR",
        0.6,
    ),
    (
        "coworking-days",
        "Coworking Policy",
        "coworking days",
        "How many coworking days per month does the current policy allow?",
        "Coworking days per month",
        "4 days",
        "8 days",
        "12 days",
        0.3,
    ),
    (
        "sabbatical",
        "Sabbatical Policy",
        "sabbatical entitlement",
        "How many sabbatical weeks does the current policy provide?",
        "Sabbatical entitlement",
        "4 weeks",
        "6 weeks",
        "8 weeks",
        0.9,
    ),
    (
        "volunteer-hours",
        "Volunteer Policy",
        "volunteer hours",
        "How many paid volunteer hours does the current policy provide?",
        "Paid volunteer hours per year",
        "8 hours",
        "16 hours",
        "24 hours",
        0.6,
    ),
    (
        "stock-vesting",
        "Equity Policy",
        "vesting period",
        "What is the current equity vesting period?",
        "Vesting period",
        "48 months",
        "36 months",
        "24 months",
        0.3,
    ),
    (
        "bonus-target",
        "Bonus Policy",
        "bonus target",
        "What is the current annual bonus target?",
        "Annual bonus target",
        "10 percent",
        "12 percent",
        "15 percent",
        0.9,
    ),
    (
        "commission-rate",
        "Commission Policy",
        "commission rate",
        "What is the current sales commission rate?",
        "Sales commission rate",
        "3 percent",
        "4 percent",
        "5 percent",
        0.6,
    ),
    (
        "discount-ceiling",
        "Discount Policy",
        "discount ceiling",
        "What discount may a representative approve under the current policy?",
        "Representative discount ceiling",
        "10 percent",
        "15 percent",
        "20 percent",
        0.3,
    ),
    (
        "refund-window",
        "Refund Policy",
        "refund window",
        "How long is the current refund window?",
        "Refund window",
        "30 days",
        "45 days",
        "60 days",
        0.9,
    ),
    (
        "warranty",
        "Warranty Policy",
        "warranty period",
        "What is the current warranty period?",
        "Warranty period",
        "12 months",
        "24 months",
        "36 months",
        0.6,
    ),
    (
        "shipping-threshold",
        "Shipping Policy",
        "free shipping threshold",
        "What is the current free shipping threshold?",
        "Free shipping threshold",
        "50 EUR",
        "75 EUR",
        "100 EUR",
        0.3,
    ),
    (
        "loyalty-points",
        "Loyalty Policy",
        "loyalty points rate",
        "How many loyalty points does the current programme give per euro?",
        "Loyalty points per euro",
        "1 point",
        "2 points",
        "3 points",
        0.9,
    ),
    (
        "seat-price",
        "Seat Pricing Policy",
        "standard seat price",
        "What is the current standard seat price?",
        "Standard seat price",
        "8 EUR",
        "10 EUR",
        "12 EUR",
        0.6,
    ),
    (
        "enterprise-seat-price",
        "Enterprise Pricing Policy",
        "enterprise seat price",
        "What is the current enterprise seat price?",
        "Enterprise seat price",
        "20 EUR",
        "24 EUR",
        "30 EUR",
        0.3,
    ),
    (
        "onboarding",
        "Onboarding Policy",
        "onboarding length",
        "How long is the current onboarding programme?",
        "Onboarding length",
        "4 weeks",
        "3 weeks",
        "2 weeks",
        0.9,
    ),
    (
        "support-tiers",
        "Support Plan Policy",
        "support tiers",
        "How many support tiers does the current plan offer?",
        "Support tiers",
        "2 tiers",
        "3 tiers",
        "4 tiers",
        0.6,
    ),
    (
        "retention-bonus",
        "Retention Policy",
        "retention bonus",
        "What is the current retention bonus?",
        "Retention bonus",
        "5 percent",
        "8 percent",
        "10 percent",
        0.3,
    ),
    (
        "pension-match",
        "Pension Policy",
        "pension match",
        "What is the current pension match?",
        "Employer pension match",
        "3 percent",
        "4 percent",
        "6 percent",
        0.9,
    ),
    (
        "mileage-rate",
        "Mileage Policy",
        "mileage rate",
        "What is the current mileage rate?",
        "Mileage rate",
        "0.30 EUR",
        "0.35 EUR",
        "0.40 EUR",
        0.6,
    ),
    (
        "per-diem",
        "Travel Per Diem Policy",
        "per diem",
        "What is the current daily travel per diem?",
        "Daily per diem",
        "40 EUR",
        "50 EUR",
        "65 EUR",
        0.3,
    ),
    (
        "parking-fee",
        "Parking Policy",
        "monthly parking fee",
        "What is the current monthly parking fee?",
        "Monthly parking fee",
        "30 EUR",
        "45 EUR",
        "60 EUR",
        0.9,
    ),
)


def families() -> list[Family]:
    out = []
    for row in RAW_FAMILIES:
        (
            slug,
            title,
            topic,
            question,
            fact,
            v2023,
            v2024,
            v2025,
            authority,
        ) = row
        out.append(
            Family(
                slug=slug,
                title=title,
                topic=topic,
                question=question,
                fact=fact,
                values=(v2023, v2024, v2025),
                authority=authority,
            )
        )
    return out


def edition_date(index: int, year: int) -> str:
    """A deterministic valid ISO date per family and year."""
    month = 1 + index % 12
    day = 1 + index % 27
    return f"{year:04d}-{month:02d}-{day:02d}"


def document(family: Family, index: int, version: int) -> str:
    year = YEARS[version]
    value = family.values[version]
    previous = "the earlier edition" if version == 0 else f"the {YEARS[version - 1]} edition"
    return (
        f"# {family.title} ({year} edition)\n"
        f"\n"
        f"This edition of the {family.title.lower()} takes effect on "
        f"{edition_date(index, year)} and replaces {previous}.\n"
        f"\n"
        f"- {family.fact}: {value}\n"
        f"\n"
        f"The figure applies to every office and region. The {family.topic} "
        f"in this edition supersedes any older statement of the same rule. "
        f"Questions about this policy go to the people team.\n"
    )


def write_set(out: Path) -> None:
    corpus = out / "corpus"
    metadata = out / "metadata"
    qrels = out / "qrels"
    for directory in (corpus, metadata, qrels):
        if directory.exists():
            shutil.rmtree(directory)
        directory.mkdir(parents=True)

    query_lines: list[str] = []
    query_tsv: list[str] = ["qid\tquery"]
    # Three-column BEIR/TREC qrels (qid, document, relevance), the shape every
    # other set in `kb-eval` uses and the one `score_deltas.py` reads. A
    # four-column TREC file (`qid 0 document rel`) is accepted by the runner
    # only with a header row, and the MRR script cannot read it, so this set
    # does not use it.
    qrel_lines: list[str] = [
        "# qid\tdocument\trelevance",
        "# Generated by generate.py. Only the newest edition is judged relevant.",
    ]

    for index, family in enumerate(families()):
        qid = f"q{index + 1:03d}"
        query_lines.append(json.dumps({"_id": qid, "text": family.question}))
        query_tsv.append(f"{qid}\t{family.question}")
        for version, year in enumerate(YEARS):
            name = f"{family.slug}-v{year}"
            (corpus / f"{name}.md").write_text(
                document(family, index, version), encoding="utf-8"
            )
            sidecar = {"effectiveDate": edition_date(index, year)}
            if family.authority is not None:
                sidecar["authority"] = family.authority
            (metadata / f"{name}.json").write_text(
                json.dumps(sidecar, indent=2) + "\n", encoding="utf-8"
            )
        qrel_lines.append(f"{qid}\t{family.slug}-v2025\t1")

    (out / "queries.jsonl").write_text("\n".join(query_lines) + "\n", encoding="utf-8")
    (out / "queries.tsv").write_text("\n".join(query_tsv) + "\n", encoding="utf-8")
    (qrels / "test.tsv").write_text("\n".join(qrel_lines) + "\n", encoding="utf-8")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--out",
        type=Path,
        default=HERE,
        help="dataset directory to write (default: this script's directory)",
    )
    args = parser.parse_args()
    write_set(args.out)
    n = len(families())
    print(f"wrote {n} families ({n * len(YEARS)} documents) to {args.out}")


if __name__ == "__main__":
    main()
