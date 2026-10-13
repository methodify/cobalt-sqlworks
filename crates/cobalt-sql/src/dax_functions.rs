//! The built-in DAX function names, grouped by the categories the DAX reference uses.
//!
//! [`DAX_FUNCTION_GROUPS`] is the source of truth; [`DAX_FUNCTIONS`] is the flat, deduplicated
//! list the lexer and the completer index. Dotted names (`INFO.VIEW.TABLES`, `IF.EAGER`,
//! `STDEV.P`) are single names: the DAX lexer keeps them as one token.

use once_cell::sync::Lazy;

/// `(category, names)` — every built-in DAX function, uppercase, grouped by category.
pub static DAX_FUNCTION_GROUPS: &[(&str, &[&str])] = &[
    (
        "Aggregation",
        &[
            "APPROXIMATEDISTINCTCOUNT", "AVERAGE", "AVERAGEA", "AVERAGEX", "COUNT", "COUNTA", "COUNTAX",
            "COUNTBLANK", "COUNTROWS", "COUNTX", "DISTINCTCOUNT", "DISTINCTCOUNTNOBLANK", "MAX", "MAXA",
            "MAXX", "MIN", "MINA", "MINX", "PRODUCT", "PRODUCTX", "SUM", "SUMX",
        ],
    ),
    (
        "Date and time",
        &[
            "CALENDAR", "CALENDARAUTO", "DATE", "DATEDIFF", "DATEVALUE", "DAY", "EDATE", "EOMONTH", "HOUR",
            "MINUTE", "MONTH", "NETWORKDAYS", "NOW", "QUARTER", "SECOND", "TIME", "TIMEVALUE", "TODAY",
            "UTCNOW", "UTCTODAY", "WEEKDAY", "WEEKNUM", "YEAR", "YEARFRAC",
        ],
    ),
    (
        "Filter",
        &[
            "ALL", "ALLCROSSFILTERED", "ALLEXCEPT", "ALLNOBLANKROW", "ALLSELECTED", "CALCULATE",
            "CALCULATETABLE", "EARLIER", "EARLIEST", "FILTER", "FIRST", "INDEX", "KEEPFILTERS", "LAST",
            "LOOKUPVALUE", "LOOKUPWITHTOTALS", "MATCHBY", "MOVINGAVERAGE", "NEXT", "OFFSET", "ORDERBY",
            "PARTITIONBY", "PREVIOUS", "RANGE", "RANK", "REMOVEFILTERS", "ROWNUMBER", "RUNNINGSUM",
            "SELECTEDVALUE", "WINDOW",
        ],
    ),
    (
        "Financial",
        &[
            "ACCRINT", "ACCRINTM", "AMORDEGRC", "AMORLINC", "COUPDAYBS", "COUPDAYS", "COUPDAYSNC", "COUPNCD",
            "COUPNUM", "COUPPCD", "CUMIPMT", "CUMPRINC", "DB", "DDB", "DISC", "DOLLARDE", "DOLLARFR",
            "DURATION", "EFFECT", "FV", "INTRATE", "IPMT", "ISPMT", "MDURATION", "NOMINAL", "NPER",
            "ODDFPRICE", "ODDFYIELD", "ODDLPRICE", "ODDLYIELD", "PDURATION", "PMT", "PPMT", "PRICE",
            "PRICEDISC", "PRICEMAT", "PV", "RATE", "RECEIVED", "RRI", "SLN", "SYD", "TBILLEQ", "TBILLPRICE",
            "TBILLYIELD", "VDB", "XIRR", "XNPV", "YIELD", "YIELDDISC", "YIELDMAT",
        ],
    ),
    (
        "Information",
        &[
            "COLUMNSTATISTICS", "CONTAINS", "CONTAINSROW", "CONTAINSSTRING", "CONTAINSSTRINGEXACT",
            "CUSTOMDATA", "HASONEFILTER", "HASONEVALUE", "ISAFTER", "ISBLANK", "ISCROSSFILTERED", "ISEMPTY",
            "ISERROR", "ISEVEN", "ISFILTERED", "ISINSCOPE", "ISLOGICAL", "ISNONTEXT", "ISNUMBER", "ISODD",
            "ISONORAFTER", "ISSELECTEDMEASURE", "ISSUBTOTAL", "ISTEXT", "NONVISUAL", "SELECTEDMEASURE",
            "SELECTEDMEASUREFORMATSTRING", "SELECTEDMEASURENAME", "USERCULTURE", "USERNAME", "USEROBJECTID",
            "USERPRINCIPALNAME",
        ],
    ),
    (
        "Logical",
        &[
            "AND", "BITAND", "BITLSHIFT", "BITOR", "BITRSHIFT", "BITXOR", "COALESCE", "FALSE", "IF",
            "IF.EAGER", "IFERROR", "NOT", "OR", "SWITCH", "TRUE",
        ],
    ),
    (
        "Math and trig",
        &[
            "ABS", "ACOS", "ACOSH", "ACOT", "ACOTH", "ASIN", "ASINH", "ATAN", "ATANH", "CEILING", "CONVERT",
            "COS", "COSH", "COT", "COTH", "CURRENCY", "DEGREES", "DIVIDE", "EVEN", "EXP", "FACT", "FLOOR",
            "GCD", "INT", "ISO.CEILING", "LCM", "LN", "LOG", "LOG10", "MOD", "MROUND", "ODD", "PI", "POWER",
            "QUOTIENT", "RADIANS", "RAND", "RANDBETWEEN", "ROUND", "ROUNDDOWN", "ROUNDUP", "SIGN", "SIN",
            "SINH", "SQRT", "SQRTPI", "TAN", "TANH", "TRUNC",
        ],
    ),
    (
        "Other",
        &["BLANK", "ERROR", "EVALUATEANDLOG", "NAMEOF", "TOCSV", "TOJSON"],
    ),
    ("Parent and child", &["PATH", "PATHCONTAINS", "PATHITEM", "PATHITEMREVERSE", "PATHLENGTH"]),
    ("Relationship", &["CROSSFILTER", "RELATED", "RELATEDTABLE", "USERELATIONSHIP"]),
    (
        "Statistical",
        &[
            "BETA.DIST", "BETA.INV", "CHISQ.DIST", "CHISQ.DIST.RT", "CHISQ.INV", "CHISQ.INV.RT", "COMBIN",
            "COMBINA", "CONFIDENCE.NORM", "CONFIDENCE.T", "EXPON.DIST", "GEOMEAN", "GEOMEANX", "LINEST",
            "LINESTX", "MEDIAN", "MEDIANX", "NORM.DIST", "NORM.INV", "NORM.S.DIST", "NORM.S.INV",
            "PERCENTILE.EXC", "PERCENTILE.INC", "PERCENTILEX.EXC", "PERCENTILEX.INC", "PERMUT",
            "POISSON.DIST", "RANK.EQ", "RANKX", "SAMPLE", "SAMPLEAXISWITHLOCALMINMAX",
            "SAMPLECARTESIANPOINTSBYCOVER", "STDEV.P", "STDEV.S", "STDEVX.P", "STDEVX.S", "T.DIST",
            "T.DIST.2T", "T.DIST.RT", "T.INV", "T.INV.2T", "VAR.P", "VAR.S", "VARX.P", "VARX.S",
        ],
    ),
    (
        "Table manipulation",
        &[
            "ADDCOLUMNS", "ADDMISSINGITEMS", "CROSSJOIN", "CURRENTGROUP", "DATATABLE", "DETAILROWS",
            "DISTINCT", "EXCEPT", "FILTERS", "GENERATE", "GENERATEALL", "GENERATESERIES", "GROUPBY",
            "IGNORE", "INTERSECT", "NATURALINNERJOIN", "NATURALLEFTOUTERJOIN", "ROLLUP",
            "ROLLUPADDISSUBTOTAL", "ROLLUPGROUP", "ROLLUPISSUBTOTAL", "ROW", "SELECTCOLUMNS",
            "SUBSTITUTEWITHINDEX", "SUMMARIZE", "SUMMARIZECOLUMNS", "TOPN", "TOPNSKIP", "TREATAS", "UNION",
            "VALUES",
        ],
    ),
    (
        "Text",
        &[
            "COMBINEVALUES", "CONCATENATE", "CONCATENATEX", "EXACT", "FIND", "FIXED", "FORMAT", "LEFT",
            "LEN", "LOWER", "MID", "REPLACE", "REPT", "RIGHT", "SEARCH", "SUBSTITUTE", "TRIM", "UNICHAR",
            "UNICODE", "UPPER", "VALUE",
        ],
    ),
    (
        "Time intelligence",
        &[
            "CLOSINGBALANCEMONTH", "CLOSINGBALANCEQUARTER", "CLOSINGBALANCEYEAR", "DATEADD",
            "DATESBETWEEN", "DATESINPERIOD", "DATESMTD", "DATESQTD", "DATESYTD", "ENDOFMONTH",
            "ENDOFQUARTER", "ENDOFYEAR", "FIRSTDATE", "FIRSTNONBLANK", "FIRSTNONBLANKVALUE", "LASTDATE",
            "LASTNONBLANK", "LASTNONBLANKVALUE", "NEXTDAY", "NEXTMONTH", "NEXTQUARTER", "NEXTYEAR",
            "OPENINGBALANCEMONTH", "OPENINGBALANCEQUARTER", "OPENINGBALANCEYEAR", "PARALLELPERIOD",
            "PREVIOUSDAY", "PREVIOUSMONTH", "PREVIOUSQUARTER", "PREVIOUSYEAR", "SAMEPERIODLASTYEAR",
            "STARTOFMONTH", "STARTOFQUARTER", "STARTOFYEAR", "TOTALMTD", "TOTALQTD", "TOTALYTD",
        ],
    ),
    (
        "INFO",
        &[
            "INFO.ALTERNATEOFDEFINITIONS", "INFO.ANNOTATIONS", "INFO.ATTRIBUTEHIERARCHIES",
            "INFO.ATTRIBUTEHIERARCHYSTORAGES", "INFO.CALCDEPENDENCY", "INFO.CALCULATIONGROUPS",
            "INFO.CALCULATIONITEMS", "INFO.CATALOGS", "INFO.CHANGEDPROPERTIES",
            "INFO.COLUMNPARTITIONSTORAGES", "INFO.COLUMNPERMISSIONS", "INFO.COLUMNS", "INFO.COLUMNSTORAGES",
            "INFO.CSDLMETADATA", "INFO.CULTURES", "INFO.DATACOVERAGEDEFINITIONS", "INFO.DATASOURCES",
            "INFO.DEPENDENCIES", "INFO.DETAILROWSDEFINITIONS", "INFO.DICTIONARYSTORAGES",
            "INFO.EXCLUDEDARTIFACTS", "INFO.EXPRESSIONS", "INFO.EXTENDEDPROPERTIES",
            "INFO.FORMATSTRINGDEFINITIONS", "INFO.FUNCTIONS", "INFO.GENERALSEGMENTMAPSEGMENTMETADATASTORAGES",
            "INFO.GROUPBYCOLUMNS", "INFO.HIERARCHIES", "INFO.HIERARCHYSTORAGES", "INFO.KPIS", "INFO.LEVELS",
            "INFO.LINGUISTICMETADATA", "INFO.MEASURES", "INFO.MODEL", "INFO.OBJECTTRANSLATIONS",
            "INFO.PARQUETFILESTORAGES", "INFO.PARTITIONS", "INFO.PARTITIONSTORAGES",
            "INFO.PERSPECTIVECOLUMNS", "INFO.PERSPECTIVEHIERARCHIES", "INFO.PERSPECTIVEMEASURES",
            "INFO.PERSPECTIVES", "INFO.PERSPECTIVETABLES", "INFO.QUERYGROUPS", "INFO.REFRESHPOLICIES",
            "INFO.RELATEDCOLUMNDETAILS", "INFO.RELATIONSHIPINDEXSTORAGES", "INFO.RELATIONSHIPS",
            "INFO.RELATIONSHIPSTORAGES", "INFO.ROLEMEMBERSHIPS", "INFO.ROLES", "INFO.SEGMENTMAPSTORAGES",
            "INFO.SEGMENTSTORAGES", "INFO.STORAGEFILES", "INFO.STORAGEFOLDERS", "INFO.STORAGETABLECOLUMNS",
            "INFO.STORAGETABLECOLUMNSEGMENTS", "INFO.STORAGETABLES", "INFO.TABLEPERMISSIONS", "INFO.TABLES",
            "INFO.TABLESTORAGES", "INFO.VARIATIONS", "INFO.VIEW.COLUMNS", "INFO.VIEW.MEASURES",
            "INFO.VIEW.RELATIONSHIPS", "INFO.VIEW.TABLES",
        ],
    ),
];

/// Every built-in DAX function name (uppercase), flattened from [`DAX_FUNCTION_GROUPS`] in
/// group order, duplicates removed. Derefs to `&[&str]`.
pub static DAX_FUNCTIONS: Lazy<Vec<&'static str>> = Lazy::new(|| {
    let mut seen = std::collections::HashSet::new();
    DAX_FUNCTION_GROUPS.iter().flat_map(|(_, names)| names.iter().copied()).filter(|n| seen.insert(*n)).collect()
});

/// The category of a built-in DAX function (case-insensitive), e.g. `"Time intelligence"`.
pub fn dax_function_category(name: &str) -> Option<&'static str> {
    let upper = name.to_ascii_uppercase();
    DAX_FUNCTION_GROUPS.iter().find(|(_, names)| names.contains(&upper.as_str())).map(|(cat, _)| *cat)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_is_large_uppercase_and_categorised() {
        assert!(DAX_FUNCTIONS.len() > 350, "{}", DAX_FUNCTIONS.len());
        for n in DAX_FUNCTIONS.iter() {
            assert_eq!(*n, n.to_ascii_uppercase(), "{n}");
            assert!(n.chars().all(|c| c.is_ascii_alphanumeric() || c == '.'), "{n}");
        }
        assert_eq!(dax_function_category("sumx"), Some("Aggregation"));
        assert_eq!(dax_function_category("INFO.VIEW.TABLES"), Some("INFO"));
        assert_eq!(dax_function_category("nope"), None);
    }
}
