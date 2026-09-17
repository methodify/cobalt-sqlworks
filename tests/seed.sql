IF DB_ID('cobalt_test') IS NULL CREATE DATABASE cobalt_test;
GO
USE cobalt_test;
GO
IF OBJECT_ID('dbo.all_types') IS NOT NULL DROP TABLE dbo.all_types;
CREATE TABLE dbo.all_types (
  id int IDENTITY(1,1) PRIMARY KEY,
  c_bit bit, c_tinyint tinyint, c_smallint smallint, c_int int, c_bigint bigint,
  c_decimal decimal(18,4), c_numeric numeric(10,2), c_money money, c_smallmoney smallmoney,
  c_float float, c_real real,
  c_date date, c_time time(7), c_datetime datetime, c_datetime2 datetime2(7), c_smalldatetime smalldatetime, c_datetimeoffset datetimeoffset(7),
  c_char char(10), c_varchar varchar(50), c_nchar nchar(10), c_nvarchar nvarchar(100), c_varcharmax varchar(max), c_nvarcharmax nvarchar(max),
  c_binary binary(8), c_varbinary varbinary(50), c_varbinarymax varbinary(max),
  c_uniqueidentifier uniqueidentifier, c_xml xml, c_json nvarchar(max), c_sqlvariant sql_variant
);
INSERT INTO dbo.all_types VALUES
 (1,255,-32768,2147483647,9223372036854775807,1234567.8901,99999999.99,922337203685477.5807,214748.3647,3.141592653589793,2.5,
  '2026-09-16','13:45:30.1234567','2026-09-16 13:45:30.123','2026-09-16 13:45:30.1234567','2026-09-16 13:45:00','2026-09-16 13:45:30.1234567 -07:00',
  'abc','hello world',N'ñandú',N'こんにちは 世界','long varchar max text','long nvarchar max ✓ text',
  0x0102030405060708,0xDEADBEEF,0xCAFEBABE,
  '6F9619FF-8B86-D011-B42D-00C04FC964FF','<root><a x="1">text</a></root>','{"k":"v","n":[1,2,3],"o":{"deep":true}}',CAST(CAST(42 AS int) AS sql_variant)),
 (0,0,0,0,0,0,0,0,0,0,0,'1900-01-01','00:00:00','1900-01-01','0001-01-01','1900-01-01','0001-01-01 00:00:00 +00:00','','','','','','',0x00,0x,0x,'00000000-0000-0000-0000-000000000000','<e/>','[]',CAST(CAST('str' AS varchar(10)) AS sql_variant)),
 (NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL);
GO
IF OBJECT_ID('dbo.big') IS NOT NULL DROP TABLE dbo.big;
CREATE TABLE dbo.big (id int NOT NULL PRIMARY KEY, category varchar(10) NOT NULL, amount decimal(12,2) NOT NULL, created datetime2(0) NOT NULL, note nvarchar(60) NULL);
;WITH n AS (SELECT TOP (2000000) ROW_NUMBER() OVER (ORDER BY (SELECT NULL)) AS i FROM sys.all_columns a CROSS JOIN sys.all_columns b CROSS JOIN sys.all_columns c)
INSERT INTO dbo.big WITH (TABLOCK) SELECT i, 'CAT' + CAST(i % 17 AS varchar(2)), (i % 100000) / 7.0, DATEADD(second, i, '2020-01-01'), CASE WHEN i % 5 = 0 THEN NULL ELSE N'note ' + CAST(i AS nvarchar(12)) END FROM n;
GO
CREATE OR ALTER VIEW dbo.v_big_summary AS SELECT category, COUNT(*) AS cnt, SUM(amount) AS total FROM dbo.big GROUP BY category;
GO
CREATE OR ALTER PROCEDURE dbo.p_multi @n int = 3 AS
BEGIN
  SET NOCOUNT ON;
  PRINT 'starting p_multi';
  SELECT TOP (@n) * FROM dbo.big ORDER BY id;
  RAISERROR('informational message %d', 10, 1, @n);
  SELECT category, COUNT(*) AS cnt FROM dbo.big GROUP BY category;
  PRINT 'done';
END
GO
CREATE OR ALTER PROCEDURE dbo.p_error AS
BEGIN
  SELECT 1 AS before_error;
  SELECT 1/0 AS boom;
END
GO
CREATE OR ALTER FUNCTION dbo.f_scalar(@x int) RETURNS int AS BEGIN RETURN @x * 2 END
GO
CREATE OR ALTER FUNCTION dbo.f_tvf(@cat varchar(10)) RETURNS TABLE AS RETURN (SELECT * FROM dbo.big WHERE category = @cat)
GO
CREATE SCHEMA reports;
GO
CREATE OR ALTER VIEW reports.AllCaseDetails AS SELECT id, category, amount FROM dbo.big WHERE id < 1000;
GO
CREATE SEQUENCE dbo.seq_test START WITH 1;
GO
CREATE TYPE dbo.IdList AS TABLE (id int NOT NULL PRIMARY KEY);
GO
CREATE SYNONYM dbo.big_syn FOR dbo.big;
GO
IF OBJECT_ID('dbo.child') IS NOT NULL DROP TABLE dbo.child;
CREATE TABLE dbo.child (id int IDENTITY PRIMARY KEY, big_id int NOT NULL CONSTRAINT FK_child_big FOREIGN KEY REFERENCES dbo.big(id), qty int NOT NULL CONSTRAINT CK_qty CHECK (qty > 0), CONSTRAINT UQ_child UNIQUE (big_id, qty));
CREATE INDEX IX_child_qty ON dbo.child(qty) INCLUDE (big_id);
GO
SELECT 'seeded' AS status, (SELECT COUNT(*) FROM dbo.big) AS big_rows;
GO
